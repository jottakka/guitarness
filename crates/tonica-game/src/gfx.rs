//! Shared rendering pieces: views, clock, layout, meshes, materials, movers, labels.

use bevy::camera::visibility::RenderLayers;
use bevy::color::Alpha;
use bevy::prelude::*;

use crate::chart::Chart;
use crate::fretboard;
use crate::palette::{self, Interval};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Fretboard seen in perspective (default).
    Fret3,
    /// Six receding lanes.
    D3,
    D2,
}

impl View {
    pub fn layer(self) -> RenderLayers {
        match self {
            View::D3 => RenderLayers::layer(0),
            View::D2 => RenderLayers::layer(1),
            View::Fret3 => RenderLayers::layer(2),
        }
    }

    pub fn idx(self) -> usize {
        match self {
            View::Fret3 => 0,
            View::D3 => 1,
            View::D2 => 2,
        }
    }

    pub fn next(self) -> View {
        match self {
            View::Fret3 => View::D3,
            View::D3 => View::D2,
            View::D2 => View::Fret3,
        }
    }
}

#[derive(Resource)]
pub struct ActiveView(pub View);

/// Show fret numerals under the active zone (key F).
#[derive(Resource)]
pub struct ShowFrets(pub bool);

/// Simulated song clock in seconds; replace its writer with the audio clock later.
#[derive(Resource)]
pub struct SongClock {
    pub t: f32,
    pub running: bool,
}

/// Private material clones of one event, with their base alpha.
pub type EventMats = Vec<(Handle<StandardMaterial>, f32)>;

/// Per-view, per-event materials whose alpha follows look-ahead and queue rank.
#[derive(Resource, Default)]
pub struct EvFade {
    pub mats: [Vec<EventMats>; 3],
    pub cur: [Vec<f32>; 3],
    pub applied: [Vec<f32>; 3],
    pub last_t: f32,
}

/// Seconds ahead of the hit line where events are fully faded out.
pub fn lookahead(view: View, chart_speed: f32) -> f32 {
    match view {
        View::Fret3 => 2.5,
        View::D3 => 2.2,
        // Keep upcoming glyphs fully inside the 16:9 viewport.
        View::D2 => (4.4 - HIT_X) / (SPEED2 * chart_speed),
    }
}

pub const FADE_IN: f32 = 0.6;

pub const LANE3: f32 = 0.8;
pub const SPEED3: f32 = 9.0;
pub const LANE2: f32 = 0.6;
pub const SPEED2: f32 = 2.6;
pub const SPEED_F: f32 = 6.0;
pub const HIT_X: f32 = -4.2;
pub const GEM_LIFT: f32 = 0.25;

/// Displacement of something `dt` seconds away from the hit line.
pub fn travel(view: View, dt: f32, chart_speed: f32) -> Vec3 {
    match view {
        View::D2 => Vec3::new(dt * SPEED2 * chart_speed, 0.0, 0.0),
        View::D3 => Vec3::new(0.0, 0.0, -dt * SPEED3 * chart_speed),
        View::Fret3 => Vec3::new(0.0, 0.0, -dt * SPEED_F * chart_speed),
    }
}

pub fn travel_speed(view: View, chart_speed: f32) -> f32 {
    match view {
        View::D2 => SPEED2 * chart_speed,
        View::D3 => SPEED3 * chart_speed,
        View::Fret3 => SPEED_F * chart_speed,
    }
}

/// Static point of (string, fret) on the hit plane, before travel.
pub fn pt(view: View, string: usize, fret: i32) -> Vec3 {
    let s = string as f32;
    match view {
        View::D3 => Vec3::new((s - 2.5) * LANE3, GEM_LIFT, 0.0),
        View::D2 => Vec3::new(HIT_X, (s - 2.5) * LANE2, GEM_LIFT),
        View::Fret3 => Vec3::new(
            fretboard::fret_x(fret.max(1)),
            fretboard::string_y(string),
            0.0,
        ),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub center: Vec3,
    pub w: f32,
    pub h: f32,
}

/// Frame rectangle for strings `lo..=hi` and (fretboard only) frets `flo..=fhi`.
pub fn rect(view: View, lo: usize, hi: usize, flo: i32, fhi: i32) -> Rect {
    let a = pt(view, lo, 1);
    let b = pt(view, hi, 1);
    match view {
        View::D3 => Rect {
            center: (a + b) / 2.0,
            w: (hi - lo) as f32 * LANE3 + 0.5,
            h: 0.5,
        },
        View::D2 => Rect {
            center: (a + b) / 2.0,
            w: 0.5,
            h: (hi - lo) as f32 * LANE2 + 0.5,
        },
        View::Fret3 => {
            let x0 = fretboard::fret_pos(flo as f32 - 1.0);
            let x1 = fretboard::fret_pos(fhi as f32);
            let y0 = a.y - 0.24;
            let y1 = b.y + 0.24;
            Rect {
                center: Vec3::new((x0 + x1) / 2.0, (y0 + y1) / 2.0, 0.0),
                w: x1 - x0,
                h: y1 - y0,
            }
        }
    }
}

#[derive(Component)]
pub struct InView(pub View);

#[derive(Component)]
pub struct ViewCam(pub View);

/// Everything spawned from a chart; despawned when the chart is switched.
#[derive(Component)]
pub struct ChartEntity;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    /// Travels with its event.
    Travel,
    /// Thin rail from the travelling gem back to its landing point on the board.
    Rail,
    /// Stays on the board at the landing point until the event passes.
    Pin,
}

/// Things that travel toward the hit line. `dur > 0` makes a sustain tail along travel.
#[derive(Component)]
pub struct Mover {
    pub kind: MoveKind,
    pub t: f32,
    pub dur: f32,
    pub pos: Vec3,
    pub base: Vec3,
    pub off: Vec3,
    pub fade: bool,
}

/// Expanding ring on the hit plane right after an accented stroke lands.
#[derive(Component)]
pub struct Shock {
    pub t: f32,
    pub pos: Vec3,
    pub size: Vec2,
}

#[derive(Component)]
pub struct HitLine {
    pub base: Vec3,
    pub thick: Vec3,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IndicatorKind {
    Idle,
    Rang,
    Missing,
    Extra,
}

#[derive(Component)]
pub struct Indicator {
    pub string: usize,
    pub kind: IndicatorKind,
    pub base: Vec3,
}

#[derive(Component)]
pub struct Label {
    pub t: f32,
    pub view: View,
    pub pos: Vec3,
}

#[derive(Resource)]
pub struct Shapes {
    pub tri_down: Handle<Mesh>,
    pub tri_up: Handle<Mesh>,
    pub diamond: Handle<Mesh>,
    pub hex: Handle<Mesh>,
    pub square: Handle<Mesh>,
    pub circle: Handle<Mesh>,
    pub sphere: Handle<Mesh>,
    pub ring: Handle<Mesh>,
    pub thumb_ring: Handle<Mesh>,
    pub disc: Handle<Mesh>,
    pub cube: Handle<Mesh>,
}

pub struct ViewMats {
    pub gem: [Handle<StandardMaterial>; 6],
    pub bar: Handle<StandardMaterial>,
    pub frame_bright: Handle<StandardMaterial>,
    pub frame_dim: Handle<StandardMaterial>,
    pub gray: Handle<StandardMaterial>,
    pub ghost: Handle<StandardMaterial>,
    pub fill: Handle<StandardMaterial>,
    pub fill_gray: Handle<StandardMaterial>,
    pub lane: Handle<StandardMaterial>,
    pub beat: Handle<StandardMaterial>,
    pub beat_strong: Handle<StandardMaterial>,
    pub string: Handle<StandardMaterial>,
    pub stem: Handle<StandardMaterial>,
    pub zone: Handle<StandardMaterial>,
    pub thumb: Handle<StandardMaterial>,
    pub shock: Handle<StandardMaterial>,
    pub chev_down: Handle<StandardMaterial>,
    pub chev_up: Handle<StandardMaterial>,
    pub rang: Handle<StandardMaterial>,
    pub missing: Handle<StandardMaterial>,
    pub extra: Handle<StandardMaterial>,
    pub idle: Handle<StandardMaterial>,
    pub hit: Handle<StandardMaterial>,
}

#[derive(Resource)]
pub struct Mats {
    pub v3: ViewMats,
    pub v2: ViewMats,
    pub vf: ViewMats,
}

impl Mats {
    pub fn of(&self, view: View) -> &ViewMats {
        match view {
            View::D3 => &self.v3,
            View::D2 => &self.v2,
            View::Fret3 => &self.vf,
        }
    }
}

#[derive(Resource)]
pub struct FlashMats {
    pub m3: Handle<StandardMaterial>,
    pub m2: Handle<StandardMaterial>,
    pub mf: Handle<StandardMaterial>,
}

fn glow_a(color: Color, k: f32, alpha: f32, view: View) -> StandardMaterial {
    let c = color.to_linear();
    let hot = LinearRgba::new(c.red * k, c.green * k, c.blue * k, alpha);
    let mode = if alpha < 1.0 {
        AlphaMode::Blend
    } else {
        AlphaMode::Opaque
    };
    match view {
        View::D3 | View::Fret3 => StandardMaterial {
            base_color: Color::LinearRgba(LinearRgba::new(
                c.red * 0.12,
                c.green * 0.12,
                c.blue * 0.12,
                alpha,
            )),
            emissive: hot,
            perceptual_roughness: 0.7,
            alpha_mode: mode,
            ..default()
        },
        View::D2 => StandardMaterial {
            base_color: Color::LinearRgba(hot),
            unlit: true,
            alpha_mode: mode,
            ..default()
        },
    }
}

fn glow(color: Color, k: f32, view: View) -> StandardMaterial {
    glow_a(color, k, 1.0, view)
}

fn view_mats(view: View, m: &mut Assets<StandardMaterial>) -> ViewMats {
    let gem = Interval::ALL.map(|i| {
        let k = if i == Interval::Other { 0.9 } else { 3.2 };
        m.add(glow(palette::interval_color(i), k, view))
    });
    let gray = Color::srgb(0.55, 0.58, 0.66);
    ViewMats {
        gem,
        bar: m.add(glow(palette::BAR, 1.1, view)),
        frame_bright: m.add(glow(Color::srgb(0.95, 0.97, 1.0), 1.7, view)),
        frame_dim: m.add(glow(palette::BAR, 0.45, view)),
        gray: m.add(glow(gray, 0.9, view)),
        ghost: m.add(glow(palette::BAR, 1.0, view)),
        fill: m.add(glow_a(palette::BAR, 0.5, 0.08, view)),
        fill_gray: m.add(glow_a(gray, 0.4, 0.07, view)),
        lane: m.add(glow(palette::LANE, 0.6, view)),
        beat: m.add(glow(palette::LANE, 0.22, view)),
        beat_strong: m.add(glow(palette::LANE, 0.55, view)),
        string: m.add(glow(Color::srgb(0.82, 0.90, 1.0), 1.1, view)),
        stem: m.add(glow(palette::LANE, 0.7, view)),
        zone: m.add(glow_a(Color::srgb(0.35, 0.55, 1.0), 0.8, 0.05, view)),
        thumb: m.add(glow(Color::srgb(0.9, 0.95, 1.0), 2.2, view)),
        shock: m.add(glow(palette::HIT, 1.8, view)),
        chev_down: m.add(glow(palette::HIT, 2.2, view)),
        chev_up: m.add(glow(Color::srgb(0.35, 0.55, 0.85), 1.8, view)),
        rang: m.add(glow(palette::HIT, 1.1, view)),
        missing: m.add(glow(Color::srgb(0.7, 0.75, 0.85), 1.0, view)),
        extra: m.add(glow(Color::srgb(0.85, 0.87, 0.92), 1.6, view)),
        idle: m.add(glow(palette::LANE, 0.5, view)),
        hit: m.add(glow(palette::HIT, 0.7, view)),
    }
}

pub fn setup_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let d = 0.3;
    let tri_down = Triangle2d::new(
        Vec2::new(-0.5, 0.4),
        Vec2::new(0.5, 0.4),
        Vec2::new(0.0, -0.5),
    );
    let tri_up = Triangle2d::new(
        Vec2::new(-0.5, -0.4),
        Vec2::new(0.0, 0.5),
        Vec2::new(0.5, -0.4),
    );
    let shapes = Shapes {
        tri_down: meshes.add(Extrusion::new(tri_down, d)),
        tri_up: meshes.add(Extrusion::new(tri_up, d)),
        diamond: meshes.add(Extrusion::new(Rhombus::new(0.8, 1.1), d)),
        hex: meshes.add(Extrusion::new(RegularPolygon::new(0.52, 6), d)),
        square: meshes.add(Extrusion::new(Rectangle::new(0.7, 0.7), d)),
        circle: meshes.add(Extrusion::new(Circle::new(0.5), d)),
        sphere: meshes.add(Sphere::new(0.5).mesh().ico(4).expect("ico sphere")),
        ring: meshes.add(Extrusion::new(Annulus::new(0.45, 0.5), 0.1)),
        thumb_ring: meshes.add(Extrusion::new(Annulus::new(0.44, 0.5), 0.1)),
        disc: meshes.add(Extrusion::new(Circle::new(0.5), 0.1)),
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
    };
    let v3 = view_mats(View::D3, &mut materials);
    let v2 = view_mats(View::D2, &mut materials);
    let vf = view_mats(View::Fret3, &mut materials);
    commands.insert_resource(FlashMats {
        m3: v3.hit.clone(),
        m2: v2.hit.clone(),
        mf: vf.hit.clone(),
    });
    commands.insert_resource(Mats { v3, v2, vf });
    commands.insert_resource(shapes);
}

pub fn update_movers(
    clock: Res<SongClock>,
    chart: Res<Chart>,
    mut q: Query<(&Mover, &InView, &mut Transform)>,
) {
    let now = clock.t;
    for (m, v, mut tf) in &mut q {
        let age = now - m.t;
        if m.fade && age > 0.26 {
            // Past the hit line: gone, not a speck.
            tf.scale = Vec3::ZERO;
            continue;
        }
        let k = 1.0;
        match m.kind {
            MoveKind::Pin => {
                tf.translation = m.pos + m.off;
                tf.scale = m.base * k;
                continue;
            }
            MoveKind::Rail => {
                let z = travel(v.0, m.t - now, chart.speed).z.min(0.0);
                tf.translation = m.pos + m.off + Vec3::new(0.0, 0.0, z / 2.0);
                tf.scale = Vec3::new(m.base.x, m.base.y, (-z).max(0.001)) * Vec3::new(k, k, 1.0);
                continue;
            }
            MoveKind::Travel => {}
        }
        let mut tc = m.t;
        let mut len = 0.0;
        if m.dur > 0.0 {
            let a = m.t.max(now);
            let b = m.t + m.dur;
            if b > a {
                tc = (a + b) / 2.0;
                len = b - a;
            }
        }
        tf.translation = m.pos + m.off + travel(v.0, tc - now, chart.speed);
        tf.scale = m.base * k;
        if m.dur > 0.0 {
            let l = (len * travel_speed(v.0, chart.speed)).max(0.001);
            match v.0 {
                View::D2 => tf.scale.x = l,
                _ => tf.scale.z = l,
            }
        }
    }
}

/// Fade events by look-ahead and (fretboard view) by queue rank: next full, then 40%, then 15%.
pub fn update_event_fade(
    clock: Res<SongClock>,
    chart: Res<Chart>,
    active: Res<ActiveView>,
    time: Res<Time<Real>>,
    mut fade: ResMut<EvFade>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let view = active.0;
    let vi = view.idx();
    let n = chart.events.len();
    if fade.mats[vi].len() != n || fade.cur[vi].len() != n {
        return;
    }
    let now = clock.t;
    let jumped = (now - fade.last_t).abs() > 0.5;
    fade.last_t = now;
    let look = lookahead(view, chart.speed);
    let r0 = chart
        .events
        .iter()
        .position(|e| e.t >= now - 0.05)
        .unwrap_or(n);
    let ease = 1.0 - (-10.0 * time.delta_secs()).exp();
    for i in 0..n {
        let dt = chart.events[i].t - now;
        let by_time = if dt <= 0.0 {
            // Past the hit line: fade out quickly, then the movers hide.
            ((0.25 + dt) / 0.25).clamp(0.0, 1.0)
        } else {
            ((look - dt) / FADE_IN).clamp(0.0, 1.0)
        };
        let by_rank = if view == View::Fret3 && i >= r0 {
            match i - r0 {
                0 => 1.0,
                1 => 0.4,
                _ => 0.15,
            }
        } else {
            1.0
        };
        let target = by_time * by_rank;
        let cur = &mut fade.cur[vi][i];
        if jumped {
            *cur = target;
        } else {
            *cur += (target - *cur) * ease;
        }
        let a = if (*cur - target).abs() < 0.004 {
            target
        } else {
            *cur
        };
        *cur = a;
        if (fade.applied[vi][i] - a).abs() < 0.002 {
            continue;
        }
        fade.applied[vi][i] = a;
        for (h, base) in &fade.mats[vi][i] {
            if let Some(mut m) = mats.get_mut(h) {
                m.base_color.set_alpha(base * a);
                m.alpha_mode = if a > 0.999 && *base >= 1.0 {
                    AlphaMode::Opaque
                } else {
                    AlphaMode::Blend
                };
            }
        }
    }
}

/// Point UI text (chord symbols, fret numerals) at the camera of the active view.
#[allow(clippy::type_complexity)]
pub fn ui_target(
    mut commands: Commands,
    view: Res<ActiveView>,
    cams: Query<(Entity, &ViewCam)>,
    ui: Query<Entity, Or<(With<Label>, With<crate::fretboard::FretNum>)>>,
    added: Query<Entity, Or<(Added<Label>, Added<crate::fretboard::FretNum>)>>,
) {
    if !view.is_changed() && added.is_empty() {
        return;
    }
    let Some((cam, _)) = cams.iter().find(|(_, v)| v.0 == view.0) else {
        return;
    };
    for e in &ui {
        commands.entity(e).insert(UiTargetCamera(cam));
    }
}

pub fn update_shocks(clock: Res<SongClock>, mut q: Query<(&Shock, &mut Transform)>) {
    for (s, mut tf) in &mut q {
        let age = clock.t - s.t;
        tf.translation = s.pos;
        if (0.0..0.4).contains(&age) {
            let k = 1.0 + age * 3.5;
            tf.scale = Vec3::new(s.size.x * k, s.size.y * k, 0.3);
        } else {
            tf.scale = Vec3::splat(0.0001);
        }
    }
}

#[allow(clippy::type_complexity)]
pub fn update_labels(
    clock: Res<SongClock>,
    chart: Res<Chart>,
    view: Res<ActiveView>,
    cams: Query<(&Camera, &GlobalTransform, &ViewCam)>,
    mut labels: Query<(Entity, &Label, &mut Node, &mut Visibility, &mut TextColor)>,
) {
    let Some((cam, gt, _)) = cams.iter().find(|(_, _, v)| v.0 == view.0) else {
        return;
    };
    let look = lookahead(view.0, chart.speed);
    // Candidates in time order; a later symbol is hidden while it would overlap a shown one.
    let mut cand: Vec<(f32, Entity, Vec2)> = Vec::new();
    for (e, l, ..) in labels.iter() {
        let dt = l.t - clock.t;
        if l.view != view.0 || !(0.0..look).contains(&dt) {
            continue;
        }
        let world = l.pos + travel(view.0, dt, chart.speed);
        if let Ok(p) = cam.world_to_viewport(gt, world)
            && p.y > 70.0
        {
            cand.push((dt, e, p));
        }
    }
    cand.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut shown: Vec<(f32, Entity, Vec2)> = Vec::new();
    for c in cand {
        if shown
            .iter()
            .all(|s| (s.2.x - c.2.x).abs() > 80.0 || (s.2.y - c.2.y).abs() > 40.0)
        {
            shown.push(c);
        }
    }
    for (e, _, mut node, mut vis, mut color) in &mut labels {
        match shown.iter().find(|s| s.1 == e) {
            Some((dt, _, p)) => {
                node.left = Val::Px(p.x - 12.0);
                node.top = Val::Px(p.y - 18.0);
                let a = ((look - dt) / FADE_IN).clamp(0.0, 1.0) * 0.9;
                color.0 = Color::srgba(0.85, 0.93, 1.0, a);
                *vis = Visibility::Inherited;
            }
            None => *vis = Visibility::Hidden,
        }
    }
}
