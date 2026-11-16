//! Pulse meshes and materials, and the pieces shared by the 3D and 2D views.
//!
//! No text anywhere: every symbol is a mesh. Error marks are white or grey (saturation 0) and
//! differ from the others by shape, never by colour; there is no red in this module.

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use guitarness_core::chart::Dir;

/// Render layers of the Pulse screen (the gallery uses 0 to 2).
pub const LAYER_3D: usize = 3;
pub const LAYER_2D: usize = 4;
pub const LAYER_UI: usize = 5;

/// The two playfields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PulseView {
    /// Rings coming down a lane toward the hit line.
    #[default]
    D3,
    /// A timeline sliding left into the hit marker.
    D2,
}

impl PulseView {
    pub fn next(self) -> PulseView {
        match self {
            PulseView::D3 => PulseView::D2,
            PulseView::D2 => PulseView::D3,
        }
    }

    pub fn layer(self) -> RenderLayers {
        match self {
            PulseView::D3 => RenderLayers::layer(LAYER_3D),
            PulseView::D2 => RenderLayers::layer(LAYER_2D),
        }
    }
}

/// The marks that say "this was not heard / not judged / extra". Every one is white or grey.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorMark {
    /// Hollow grey ring: an expected stroke that was not heard.
    NotHeardRing,
    /// Small white dash: a stroke nobody expected.
    ExtraDash,
    /// The lock of a stroke that was not judged.
    LockBody,
    LockShackle,
    /// The faded ring under a lock, a ghost or a contested mark.
    DimRing,
    /// The neutral dot of a contested mark.
    DisputeDot,
}

impl ErrorMark {
    pub const ALL: [ErrorMark; 6] = [
        ErrorMark::NotHeardRing,
        ErrorMark::ExtraDash,
        ErrorMark::LockBody,
        ErrorMark::LockShackle,
        ErrorMark::DimRing,
        ErrorMark::DisputeDot,
    ];

    /// Grey level (0..=1, equal on all three channels) and glow factor.
    pub fn grey(self) -> (f32, f32) {
        match self {
            ErrorMark::NotHeardRing => (0.36, 1.0),
            ErrorMark::ExtraDash => (0.92, 1.6),
            ErrorMark::LockBody => (0.62, 1.0),
            ErrorMark::LockShackle => (0.62, 1.0),
            ErrorMark::DimRing => (0.34, 1.0),
            ErrorMark::DisputeDot => (0.72, 1.0),
        }
    }

    pub fn material(self) -> StandardMaterial {
        let (g, k) = self.grey();
        unlit(Color::linear_rgb(g * k, g * k, g * k))
    }
}

fn unlit(c: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: c,
        unlit: true,
        ..default()
    }
}

fn glow(r: f32, g: f32, b: f32, k: f32) -> StandardMaterial {
    unlit(Color::linear_rgb(r * k, g * k, b * k))
}

/// Meshes (flat, facing +Z; the 3D view lays them on the lane by rotating the event root).
#[derive(Resource, Clone)]
pub struct PulseShapes {
    pub ring_thin: Handle<Mesh>,
    pub ring_thick: Handle<Mesh>,
    pub disc: Handle<Mesh>,
    pub tri_down: Handle<Mesh>,
    pub tri_up: Handle<Mesh>,
    pub tri_right: Handle<Mesh>,
    pub diamond: Handle<Mesh>,
    pub bar: Handle<Mesh>,
}

#[derive(Resource, Clone)]
pub struct PulseMats {
    /// Rings waiting for their stroke.
    pub ring: Handle<StandardMaterial>,
    pub ring_strong: Handle<StandardMaterial>,
    /// The guide arrow (stroke direction) before the stroke.
    pub guide: Handle<StandardMaterial>,
    /// The arrow after the stroke was heard.
    pub fill: Handle<StandardMaterial>,
    /// "Center" glow, only with a measured loop.
    pub center: Handle<StandardMaterial>,
    pub hit: Handle<StandardMaterial>,
    pub edge: Handle<StandardMaterial>,
    pub grid: Handle<StandardMaterial>,
    pub grid_strong: Handle<StandardMaterial>,
    pub dot_off: Handle<StandardMaterial>,
    pub dot_on: Handle<StandardMaterial>,
    pub chip: Handle<StandardMaterial>,
    /// Background colour, to hide lines behind a glyph.
    pub mask: Handle<StandardMaterial>,
    pub errors: [Handle<StandardMaterial>; 6],
}

impl PulseMats {
    pub fn error(&self, m: ErrorMark) -> &Handle<StandardMaterial> {
        &self.errors[ErrorMark::ALL.iter().position(|x| *x == m).unwrap_or(0)]
    }
}

pub fn build_shapes(meshes: &mut Assets<Mesh>) -> PulseShapes {
    let d = 0.02;
    let down = Triangle2d::new(
        Vec2::new(-0.5, 0.4),
        Vec2::new(0.5, 0.4),
        Vec2::new(0.0, -0.5),
    );
    let up = Triangle2d::new(
        Vec2::new(-0.5, -0.4),
        Vec2::new(0.0, 0.5),
        Vec2::new(0.5, -0.4),
    );
    let right = Triangle2d::new(
        Vec2::new(-0.35, 0.5),
        Vec2::new(-0.35, -0.5),
        Vec2::new(0.55, 0.0),
    );
    PulseShapes {
        ring_thin: meshes.add(Extrusion::new(Annulus::new(0.465, 0.5), d)),
        ring_thick: meshes.add(Extrusion::new(Annulus::new(0.38, 0.5), d)),
        disc: meshes.add(Extrusion::new(Circle::new(0.5), d)),
        tri_down: meshes.add(Extrusion::new(down, d)),
        tri_up: meshes.add(Extrusion::new(up, d)),
        tri_right: meshes.add(Extrusion::new(right, d)),
        diamond: meshes.add(Extrusion::new(Rhombus::new(0.9, 0.9), d)),
        bar: meshes.add(Extrusion::new(Rectangle::new(1.0, 1.0), d)),
    }
}

pub fn build_mats(m: &mut Assets<StandardMaterial>) -> PulseMats {
    let errors = ErrorMark::ALL.map(|e| m.add(e.material()));
    PulseMats {
        ring: m.add(glow(0.62, 0.78, 1.0, 1.1)),
        ring_strong: m.add(glow(0.85, 0.93, 1.0, 1.7)),
        guide: m.add(glow(0.40, 0.52, 0.75, 0.9)),
        fill: m.add(glow(0.92, 0.96, 1.0, 2.4)),
        center: m.add(glow(0.55, 0.85, 1.0, 3.0)),
        hit: m.add(glow(0.80, 0.92, 1.0, 1.5)),
        edge: m.add(glow(0.30, 0.42, 0.65, 0.9)),
        grid: m.add(glow(0.30, 0.38, 0.55, 0.45)),
        grid_strong: m.add(glow(0.45, 0.58, 0.85, 1.0)),
        dot_off: m.add(glow(0.30, 0.38, 0.55, 0.5)),
        dot_on: m.add(glow(0.85, 0.93, 1.0, 2.0)),
        chip: m.add(glow(0.55, 0.70, 0.95, 1.0)),
        mask: m.add(unlit(crate::palette::BG)),
        errors,
    }
}

/// Which part of an event a child entity is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    /// The ring (thin; the downbeat has a second one).
    Ring,
    /// Inner ring, downbeats only.
    Ring2,
    /// Dim guide arrow of the stroke direction.
    Guide,
    /// Bright arrow, filled by the attack and shifted by the timing.
    Fill,
    /// Hollow grey ring.
    NotHeard,
    /// Padlock (body and shackle).
    Lock,
    /// Faded ring.
    Dim,
    /// Glow behind a centred stroke.
    Center,
    /// Neutral dot of a contested mark.
    Dispute,
}

/// Root of one expected stroke in one view.
#[derive(Component, Clone, Copy, Debug)]
pub struct PulseEv {
    pub id: usize,
    /// Seconds after chart time zero.
    pub t: f32,
    pub view: PulseView,
}

/// A child of a [`PulseEv`].
#[derive(Component, Clone, Copy, Debug)]
pub struct Part {
    pub id: usize,
    pub kind: PartKind,
    pub view: PulseView,
}

/// Beat line across the lane (the visual metronome).
#[derive(Component, Clone, Copy, Debug)]
pub struct PulseGrid {
    pub t: f32,
    pub strong: bool,
    pub view: PulseView,
}

/// A slot of a small pool: stray attacks (arrow with no stroke) and extra strokes.
#[derive(Component, Clone, Copy, Debug)]
pub struct PoolMark {
    pub extra: bool,
    pub slot: usize,
    pub view: PulseView,
}

/// Everything spawned for one round; despawned with the next.
#[derive(Component)]
pub struct RoundEntity(pub PulseView);

pub const POOL: usize = 8;

/// Local offset of the shifted arrow, for full shift (the window edge), in world units along
/// the time axis.
pub const MAX_SHIFT: f32 = 0.55;

/// Diameter of a ring: the downbeat is bigger (and has a second ring), eighths are smaller.
pub fn ring_size(strong: bool, small: bool) -> f32 {
    if strong {
        1.9
    } else if small {
        0.95
    } else {
        1.45
    }
}

/// Spawns one expected stroke: root plus parts. `root_rot` lays the flat parts on the lane
/// (3D) or leaves them facing the camera (2D). All parts start hidden except the ring and
/// the guide arrow.
#[allow(clippy::too_many_arguments)]
pub fn spawn_event(
    commands: &mut Commands,
    shapes: &PulseShapes,
    mats: &PulseMats,
    view: PulseView,
    id: usize,
    t: f32,
    dir: Dir,
    strong: bool,
    small: bool,
    root_rot: Quat,
) {
    let size = ring_size(strong, small);
    let arrow = match dir {
        Dir::Down => shapes.tri_down.clone(),
        Dir::Up => shapes.tri_up.clone(),
    };
    let layer = view.layer();
    let ring_mat = if strong {
        mats.ring_strong.clone()
    } else {
        mats.ring.clone()
    };
    let part = |kind| Part { id, kind, view };
    commands
        .spawn((
            Transform::from_rotation(root_rot),
            Visibility::Hidden,
            PulseEv { id, t, view },
            RoundEntity(view),
            layer.clone(),
        ))
        .with_children(|c| {
            let flat =
                |s: f32, z: f32| Transform::from_xyz(0.0, 0.0, z).with_scale(Vec3::new(s, s, 1.0));
            c.spawn((
                // Thin for every beat: the downbeat is told apart by size and the second ring,
                // so it does not outshine the playhead.
                Mesh3d(shapes.ring_thin.clone()),
                MeshMaterial3d(ring_mat.clone()),
                flat(size, 0.0),
                part(PartKind::Ring),
                layer.clone(),
            ));
            if view == PulseView::D2 {
                // Hides the beat tick behind the glyph.
                c.spawn((
                    Mesh3d(shapes.disc.clone()),
                    MeshMaterial3d(mats.mask.clone()),
                    flat(size * 0.94, -0.001),
                    layer.clone(),
                ));
            }
            c.spawn((
                Mesh3d(shapes.ring_thin.clone()),
                MeshMaterial3d(ring_mat),
                flat(size * 0.62, 0.001),
                Visibility::Hidden,
                part(PartKind::Ring2),
                layer.clone(),
            ));
            c.spawn((
                Mesh3d(arrow.clone()),
                MeshMaterial3d(mats.guide.clone()),
                flat(size * 0.34, 0.002),
                part(PartKind::Guide),
                layer.clone(),
            ));
            c.spawn((
                Mesh3d(arrow),
                MeshMaterial3d(mats.fill.clone()),
                flat(size * 0.46, 0.004),
                Visibility::Hidden,
                part(PartKind::Fill),
                layer.clone(),
            ));
            c.spawn((
                Mesh3d(shapes.disc.clone()),
                MeshMaterial3d(mats.center.clone()),
                flat(size * 1.15, -0.002),
                Visibility::Hidden,
                part(PartKind::Center),
                layer.clone(),
            ));
            c.spawn((
                // Quieter than a waiting ring: thin and dim grey, never an accent.
                Mesh3d(shapes.ring_thin.clone()),
                MeshMaterial3d(mats.error(ErrorMark::NotHeardRing).clone()),
                flat(size * 0.94, 0.003),
                Visibility::Hidden,
                part(PartKind::NotHeard),
                layer.clone(),
            ));
            c.spawn((
                Mesh3d(shapes.ring_thin.clone()),
                MeshMaterial3d(mats.error(ErrorMark::DimRing).clone()),
                flat(size, 0.003),
                Visibility::Hidden,
                part(PartKind::Dim),
                layer.clone(),
            ));
            c.spawn((
                Mesh3d(shapes.disc.clone()),
                MeshMaterial3d(mats.error(ErrorMark::DisputeDot).clone()),
                Transform::from_xyz(0.0, 0.0, 0.004).with_scale(Vec3::new(
                    size * 0.14,
                    size * 0.14,
                    1.0,
                )),
                Visibility::Hidden,
                part(PartKind::Dispute),
                layer.clone(),
            ));
            // The lock: a body and a shackle, grey.
            let s = size * 0.3;
            c.spawn((
                Transform::from_xyz(0.0, 0.0, 0.004),
                Visibility::Hidden,
                part(PartKind::Lock),
                layer.clone(),
            ))
            .with_children(|l| {
                l.spawn((
                    Mesh3d(shapes.bar.clone()),
                    MeshMaterial3d(mats.error(ErrorMark::LockBody).clone()),
                    Transform::from_xyz(0.0, -0.08 * s, 0.0).with_scale(Vec3::new(
                        1.0 * s,
                        0.75 * s,
                        1.0,
                    )),
                    layer.clone(),
                ));
                l.spawn((
                    Mesh3d(shapes.ring_thick.clone()),
                    MeshMaterial3d(mats.error(ErrorMark::LockShackle).clone()),
                    Transform::from_xyz(0.0, 0.42 * s, 0.0).with_scale(Vec3::new(
                        0.62 * s,
                        0.62 * s,
                        1.0,
                    )),
                    layer.clone(),
                ));
            });
        });
}

/// Scale of a beat line of the visual metronome.
pub fn grid_scale(strong: bool, width: f32, view: PulseView) -> Vec3 {
    let thick = if strong { 0.05 } else { 0.022 };
    match view {
        // Across the lane (x), thin along the lane (z): lies flat after the 3D rotation.
        PulseView::D3 => Vec3::new(width, thick, 1.0),
        PulseView::D2 => Vec3::new(thick, width, 1.0),
    }
}

/// A saturation of 0 means equal red, green and blue.
pub fn saturation(c: Color) -> f32 {
    let l = c.to_linear();
    let mx = l.red.max(l.green).max(l.blue);
    let mn = l.red.min(l.green).min(l.blue);
    if mx <= 0.0 { 0.0 } else { (mx - mn) / mx }
}

/// The colours of a material that can show on screen (base and emissive).
pub fn material_colors(m: &StandardMaterial) -> [Color; 2] {
    [m.base_color, Color::from(m.emissive)]
}
