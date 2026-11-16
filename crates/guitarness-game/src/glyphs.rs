//! Spawns gems and stroke glyphs for a chart, for any view.
//!
//! Every glyph lives in the XY plane facing +Z and travels toward the hit plane,
//! so one set of builders serves the fretboard, lanes and 2D views.

use std::collections::HashMap;

use bevy::color::Alpha;
use bevy::prelude::*;

use crate::chart::{Chart, Dir, Event, Glyph, Kind, Style};
use crate::fretboard;
use crate::gfx::{
    self, ChartEntity, EvFade, InView, Indicator, IndicatorKind, Label, Mats, MoveKind, Mover,
    Shapes, Shock, View, ViewMats,
};
use crate::palette::Interval;

struct Ctx<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    view: View,
    shapes: &'a Shapes,
    vm: &'a ViewMats,
    materials: &'a mut Assets<StandardMaterial>,
    /// When set, every part gets a private material clone so its event can fade alone.
    track: bool,
    cache: HashMap<AssetId<StandardMaterial>, Mat>,
    owned: Vec<(Mat, f32)>,
}

type Mat = Handle<StandardMaterial>;

impl Ctx<'_, '_, '_> {
    fn begin_event(&mut self) {
        self.track = true;
        self.cache.clear();
        self.owned.clear();
    }

    fn own(&mut self, mat: &Mat) -> Mat {
        if !self.track {
            return mat.clone();
        }
        if let Some(h) = self.cache.get(&mat.id()) {
            return h.clone();
        }
        let Some(src) = self.materials.get(mat).cloned() else {
            return mat.clone();
        };
        let base = src.base_color.alpha();
        let h = self.materials.add(src);
        self.cache.insert(mat.id(), h.clone());
        self.owned.push((h.clone(), base));
        h
    }

    /// Private, dimmed copy of a material that still fades with its event.
    fn own_dim(&mut self, mat: &Mat, mul: f32) -> Mat {
        let Some(mut src) = self.materials.get(mat).cloned() else {
            return mat.clone();
        };
        src.base_color.set_alpha(src.base_color.alpha() * mul);
        let base = src.base_color.alpha();
        let h = self.materials.add(src);
        self.owned.push((h.clone(), base));
        h
    }

    fn part(&mut self, mesh: &Handle<Mesh>, mat: &Mat, mover: Mover, rot: Quat) {
        let mat = self.own(mat);
        self.commands.spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(mat),
            Transform {
                rotation: rot,
                scale: mover.base,
                ..default()
            },
            mover,
            InView(self.view),
            self.view.layer(),
            ChartEntity,
        ));
    }

    fn mover(t: f32, pos: Vec3, base: Vec3) -> Mover {
        Mover {
            kind: MoveKind::Travel,
            t,
            dur: 0.0,
            pos,
            base,
            off: Vec3::ZERO,
            fade: true,
        }
    }

    /// Straight segment between two plane points.
    fn seg(&mut self, t: f32, a: Vec3, b: Vec3, thick: f32, mat: &Mat) {
        let d = b - a;
        let len = d.truncate().length() + thick * 0.6;
        let ang = d.y.atan2(d.x);
        let cube = self.shapes.cube.clone();
        self.part(
            &cube,
            mat,
            Self::mover(t, (a + b) / 2.0, Vec3::new(len, thick, thick * 0.7)),
            Quat::from_rotation_z(ang),
        );
    }

    fn head(&mut self, t: f32, tip: Vec3, dir: Vec3, thick: f32, mat: &Mat) {
        let back = -dir.truncate().normalize_or_zero();
        for sign in [-1.0f32, 1.0] {
            let v = Vec2::from_angle(sign * 35f32.to_radians()).rotate(back) * 0.24;
            self.seg(t, tip, tip + v.extend(0.0), thick, mat);
        }
    }

    fn arrow(&mut self, t: f32, a: Vec3, b: Vec3, head_at_b: bool, mat: &Mat, dashed: bool) {
        let thick = if dashed { 0.06 } else { 0.05 };
        if dashed {
            // Larger dashes with clear gaps; lighter than a solid arrow, still legible.
            let (dash, gap) = (0.15, 0.1);
            let len = (b - a).length();
            let dir = (b - a) / len.max(0.001);
            let mut s0 = 0.0;
            while s0 < len {
                let s1 = (s0 + dash).min(len);
                self.seg(t, a + dir * s0, a + dir * s1, thick, mat);
                s0 += dash + gap;
            }
        } else {
            self.seg(t, a, b, thick, mat);
        }
        let (tip, dir) = if head_at_b { (b, b - a) } else { (a, a - b) };
        self.head(t, tip, dir, thick, mat);
    }

    fn curve(&mut self, t: f32, a: Vec3, b: Vec3, bulge: Vec3, head_at_b: bool, mat: &Mat) {
        let ctrl = (a + b) / 2.0 + bulge;
        let at = |u: f32| a * (1.0 - u).powi(2) + ctrl * 2.0 * u * (1.0 - u) + b * u * u;
        let n = 8;
        for i in 0..n {
            self.seg(
                t,
                at(i as f32 / n as f32),
                at((i + 1) as f32 / n as f32),
                0.045,
                mat,
            );
        }
        let (tip, dir) = if head_at_b {
            (b, b - at(0.85))
        } else {
            (a, a - at(0.15))
        };
        self.head(t, tip, dir, 0.045, mat);
    }

    fn outline(&mut self, t: f32, r: gfx::Rect, thick: f32, mat: &Mat) {
        let (hw, hh) = (r.w / 2.0, r.h / 2.0);
        let c = r.center;
        let corners = [
            c + Vec3::new(-hw, -hh, 0.0),
            c + Vec3::new(hw, -hh, 0.0),
            c + Vec3::new(hw, hh, 0.0),
            c + Vec3::new(-hw, hh, 0.0),
        ];
        for i in 0..4 {
            self.seg(t, corners[i], corners[(i + 1) % 4], thick, mat);
        }
    }

    fn fill(&mut self, t: f32, r: gfx::Rect, mat: &Mat) {
        let cube = self.shapes.cube.clone();
        let pos = r.center - Vec3::new(0.0, 0.0, 0.03);
        self.part(
            &cube,
            mat,
            Self::mover(t, pos, Vec3::new(r.w, r.h, 0.01)),
            Quat::IDENTITY,
        );
    }

    /// A few widely spaced diagonals clipped to the rectangle.
    fn hatch(&mut self, t: f32, r: gfx::Rect, step: f32, mat: &Mat) {
        let (a, b) = (r.w / 2.0, r.h / 2.0);
        // line y = x + c, clipped to the box
        let mut c = -(a + b) + step * 0.5;
        while c < a + b {
            let (x0, x1) = ((-a).max(-b - c), a.min(b - c));
            if x1 - x0 > 0.05 {
                let p0 = r.center + Vec3::new(x0, x0 + c, 0.0);
                let p1 = r.center + Vec3::new(x1, x1 + c, 0.0);
                self.seg(t, p0, p1, 0.03, mat);
            }
            c += step;
        }
    }

    /// Palm-mute stub: a fixed-length spine with cross ticks, trailing the gem.
    fn stub(&mut self, tg: f32, pos: Vec3, start: f32, mat: &Mat) {
        let cube = self.shapes.cube.clone();
        let (fwd, lat) = match self.view {
            View::D2 => (Vec3::X, Vec3::Y),
            _ => (Vec3::NEG_Z, Vec3::X),
        };
        let len = 0.38;
        let spine = fwd * len + (Vec3::ONE - fwd) * 0.05;
        let mut m = Self::mover(tg, pos, spine);
        m.off = fwd * (start + len / 2.0);
        self.part(&cube, mat, m, Quat::IDENTITY);
        for k in 0..3 {
            let tick = lat * 0.24 + (Vec3::ONE - lat) * 0.045;
            let mut m = Self::mover(tg, pos, tick);
            m.off = fwd * (start + 0.07 + 0.13 * k as f32);
            self.part(&cube, mat, m, Quat::IDENTITY);
        }
    }
}

fn shape_for(view: View, i: Interval, s: &Shapes) -> (Handle<Mesh>, f32) {
    match i {
        Interval::Root => (
            if view == View::D2 {
                s.circle.clone()
            } else {
                s.sphere.clone()
            },
            0.36,
        ),
        Interval::MinorThird => (s.tri_down.clone(), 0.38),
        Interval::MajorThird => (s.tri_up.clone(), 0.38),
        Interval::Fifth => (s.diamond.clone(), 0.38),
        Interval::MinorSeventh => (s.hex.clone(), 0.36),
        Interval::Other => (s.square.clone(), 0.2),
    }
}

/// Fret range for the frame of an event (fretboard view only).
fn fret_range(chart: &Chart, ev: &Event) -> (i32, i32) {
    let covered = chart.covered(ev);
    let chord = &chart.chords[ev.chord];
    let frets: Vec<i32> = match ev.kind {
        Kind::Note { fret, .. } => vec![fret],
        Kind::Strum { .. } => (0..6)
            .filter(|s| covered[*s])
            .map(|s| chord.frets[s])
            .collect(),
    };
    let fretted: Vec<i32> = frets.into_iter().filter(|f| *f > 0).collect();
    match (fretted.iter().min(), fretted.iter().max()) {
        (Some(lo), Some(hi)) => (*lo, *hi),
        _ => {
            let (lo, hi) =
                fretboard::zone_target(chart, ev.t - 0.3, ev.t + 1.9).unwrap_or((1.0, 4.0));
            (lo.round() as i32, hi.round() as i32)
        }
    }
}

fn event_rect(view: View, chart: &Chart, ev: &Event) -> gfx::Rect {
    let (lo, hi) = chart.span(ev).unwrap_or((0, 5));
    let (flo, fhi) = fret_range(chart, ev);
    gfx::rect(view, lo, hi, flo, fhi)
}

/// Seconds a sustain on `string` may run before the next frame or gem on that string.
fn free_time(cx: &Ctx, chart: &Chart, ev: &Event, string: usize, tg: f32) -> f32 {
    let gap = 0.5 / gfx::travel_speed(cx.view, chart.speed);
    chart
        .events
        .iter()
        .filter(|o| o.t > tg + 1e-4)
        .filter(|o| {
            chart
                .span(o)
                .is_some_and(|(lo, hi)| (lo..=hi).contains(&string))
        })
        .map(|o| o.t - tg - gap)
        .fold(ev.dur, f32::min)
}

fn spawn_gem(cx: &mut Ctx, chart: &Chart, ev: &Event, g: &Glyph, style: Option<Style>) {
    let view = cx.view;
    let tg = ev.t + g.dt;
    let gem_mat = cx.vm.gem[g.interval.index()].clone();
    let cube = cx.shapes.cube.clone();
    let mut size_mul = if matches!(ev.kind, Kind::Note { .. }) {
        1.3
    } else {
        1.0
    };
    match style {
        Some(Style::Roll) => size_mul = 0.8,
        Some(Style::PalmMute) => size_mul = 0.75,
        _ => {}
    }
    if view == View::Fret3 && g.fret == 0 {
        // Open string: a thin, dim bar clipped to the event's own frame, behind the glyphs.
        let r = event_rect(view, chart, ev);
        let pos = Vec3::new(r.center.x, fretboard::string_y(g.string), -0.06);
        let dim = cx.own_dim(&gem_mat, 0.55);
        let bar = Ctx::mover(tg, pos, Vec3::new(r.w, 0.026, 0.026));
        cx.commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(dim),
            Transform::from_translation(pos).with_scale(bar.base),
            bar,
            InView(view),
            view.layer(),
            ChartEntity,
        ));
        return;
    }
    let pos = gfx::pt(view, g.string, g.fret);
    let (mesh, size) = shape_for(view, g.interval, cx.shapes);
    let size = size * size_mul * if view == View::Fret3 { 1.3 } else { 1.0 };
    cx.part(
        &mesh,
        &gem_mat,
        Ctx::mover(tg, pos, Vec3::splat(size)),
        Quat::IDENTITY,
    );
    if view == View::Fret3 {
        // A thin rail ties the gem to its exact landing point, marked by a ring on the board.
        let stem = cx.vm.stem.clone();
        let mut rail = Ctx::mover(tg, pos, Vec3::new(0.014, 0.014, 1.0));
        rail.kind = MoveKind::Rail;
        cx.part(&cube, &stem, rail, Quat::IDENTITY);
        let ring = cx.shapes.ring.clone();
        let mut pin = Ctx::mover(tg, pos, Vec3::splat(0.34));
        pin.kind = MoveKind::Pin;
        cx.part(&ring, &stem, pin, Quat::IDENTITY);
    }
    if ev.dur > 0.0 && style.is_none() {
        // Sustain tail; it stops short of the next gem or frame on its string.
        let dur = free_time(cx, chart, ev, g.string, tg);
        if dur * gfx::travel_speed(view, chart.speed) >= 0.35 {
            let mover = Mover {
                kind: MoveKind::Travel,
                t: tg,
                dur,
                pos,
                base: Vec3::splat(0.07),
                off: Vec3::ZERO,
                fade: false,
            };
            cx.part(&cube, &gem_mat, mover, Quat::IDENTITY);
        }
    }
    if style == Some(Style::PalmMute) {
        cx.stub(tg, pos, size / 2.0 + 0.05, &gem_mat);
    }
    if g.finger == Some('p') && g.string <= 2 {
        // Thumb: a ring about 1.25x the gem, with a gap, sitting behind it.
        let ring = cx.shapes.thumb_ring.clone();
        let thumb = cx.vm.thumb.clone();
        let m = Ctx::mover(
            tg,
            pos - Vec3::new(0.0, 0.0, 0.05),
            Vec3::splat(size * 1.25),
        );
        cx.part(&ring, &thumb, m, Quat::IDENTITY);
    }
}

/// Ends of a strum arrow beside its frame: (top, bottom, side direction).
/// The arrow always stands vertical so that down reads as a down arrow in every view.
fn arrow_ends(
    view: View,
    rect: &gfx::Rect,
    lo: usize,
    hi: usize,
    stagger: f32,
) -> (Vec3, Vec3, Vec3) {
    let z = rect.center.z;
    match view {
        View::D3 => {
            let x = rect.center.x + rect.w / 2.0 + 0.22;
            (Vec3::new(x, 0.95, z), Vec3::new(x, 0.05, z), Vec3::X)
        }
        View::D2 | View::Fret3 => {
            let (ya, yb) = (gfx::pt(view, lo, 1).y, gfx::pt(view, hi, 1).y);
            let inset = if view == View::D2 { 0.1 } else { 0.05 };
            let mid = (ya + yb) / 2.0;
            let half = ((yb - ya) / 2.0 - inset).max(0.22);
            let (x, side) = if view == View::D2 {
                (rect.center.x + rect.w / 2.0 + 0.14 + stagger, Vec3::X)
            } else {
                (rect.center.x - rect.w / 2.0 - 0.16, Vec3::NEG_X)
            };
            (
                Vec3::new(x, mid + half, z),
                Vec3::new(x, mid - half, z),
                side,
            )
        }
    }
}

fn spawn_stroke(cx: &mut Ctx, chart: &Chart, ev: &Event, dir: Dir, style: Style, repeat: bool) {
    let view = cx.view;
    let Some((lo, hi)) = chart.span(ev) else {
        return;
    };
    let rect = event_rect(view, chart, ev);
    let stagger = if view == View::D2 {
        chart.glyphs(ev).last().map_or(0.0, |g| g.dt) * gfx::travel_speed(view, chart.speed)
    } else {
        0.0
    };
    let (top, bottom, side) = arrow_ends(view, &rect, lo, hi, stagger);
    // Notation: down-strum is a down arrow, up-strum an up arrow, whatever the string order.
    let down = dir == Dir::Down;
    let (a, b) = if down { (top, bottom) } else { (bottom, top) };
    let arrow_mat = if down {
        cx.vm.chev_down.clone()
    } else {
        cx.vm.chev_up.clone()
    };
    let (thin, thick) = if view == View::Fret3 {
        (0.025, 0.05)
    } else {
        (0.04, 0.065)
    };
    let t = ev.t;
    match style {
        Style::Ghost => {
            let ghost = cx.vm.ghost.clone();
            cx.arrow(t, a, b, true, &ghost, true);
        }
        Style::Mute => {
            let (gray, fill) = (cx.vm.gray.clone(), cx.vm.fill_gray.clone());
            cx.fill(t, rect, &fill);
            cx.outline(t, rect, thin, &gray);
            let step = if view == View::Fret3 { 0.8 } else { 0.6 };
            cx.hatch(t, rect, step, &gray);
        }
        Style::Roll => {
            cx.curve(t, a, b, side * 0.4, true, &arrow_mat);
        }
        Style::Accent => {
            let (bright, fill) = (cx.vm.frame_bright.clone(), cx.vm.fill.clone());
            cx.fill(t, rect, &fill);
            cx.outline(t, rect, thick, &bright);
            cx.arrow(t, a, b, true, &arrow_mat, false);
            let (ring, shock) = (cx.shapes.ring.clone(), cx.vm.shock.clone());
            let pos = rect.center - Vec3::new(0.0, 0.0, 0.05);
            let shock = cx.own(&shock);
            cx.commands.spawn((
                Mesh3d(ring),
                MeshMaterial3d(shock),
                Transform::from_translation(pos).with_scale(Vec3::splat(0.0001)),
                Shock {
                    t,
                    pos,
                    size: Vec2::new(rect.w, rect.h),
                },
                view.layer(),
                ChartEntity,
            ));
        }
        Style::Normal | Style::Bass | Style::Treble | Style::PalmMute => {
            if repeat {
                let dim = cx.vm.frame_dim.clone();
                cx.outline(t, rect, 0.02, &dim);
            } else {
                let (bar, fill) = (cx.vm.bar.clone(), cx.vm.fill.clone());
                cx.fill(t, rect, &fill);
                cx.outline(t, rect, thin, &bar);
            }
            cx.arrow(t, a, b, true, &arrow_mat, false);
        }
    }
}

/// Spawn gems, frames, arrows, labels and beat lines for one view.
pub fn spawn_chart(
    commands: &mut Commands,
    view: View,
    chart: &Chart,
    shapes: &Shapes,
    mats: &Mats,
    materials: &mut Assets<StandardMaterial>,
    fade: &mut EvFade,
) {
    let vm = mats.of(view);
    let mut cx = Ctx {
        commands,
        view,
        shapes,
        vm,
        materials,
        track: false,
        cache: HashMap::new(),
        owned: Vec::new(),
    };
    let vi = view.idx();
    fade.mats[vi].clear();
    let mut prev: Option<(usize, [bool; 6])> = None;
    for ev in &chart.events {
        cx.begin_event();
        let style = match ev.kind {
            Kind::Strum { style, .. } => Some(style),
            Kind::Note { .. } => None,
        };
        for g in chart.glyphs(ev) {
            spawn_gem(&mut cx, chart, ev, &g, style);
        }
        if let Kind::Strum { dir, style } = ev.kind {
            let same = prev == Some((ev.chord, chart.covered(ev)));
            spawn_stroke(&mut cx, chart, ev, dir, style, same);
            prev = Some((ev.chord, chart.covered(ev)));
        }
        fade.mats[vi].push(std::mem::take(&mut cx.owned));
        if let Some(name) = ev.label.as_deref() {
            let r = event_rect(view, chart, ev);
            let up = r.h / 2.0 + if view == View::D3 { 0.8 } else { 0.4 };
            cx.commands.spawn((
                Label {
                    t: ev.t,
                    view,
                    pos: r.center + Vec3::new(0.0, up, 0.0),
                },
                Text::new(name),
                TextFont {
                    font_size: FontSize::Px(30.0),
                    ..default()
                },
                TextColor(Color::srgba(0.85, 0.93, 1.0, 0.9)),
                Node {
                    position_type: PositionType::Absolute,
                    ..default()
                },
                Visibility::Hidden,
                ChartEntity,
            ));
        }
    }
    cx.track = false;
    let n = chart.events.len();
    fade.cur[vi] = vec![0.0; n];
    fade.applied[vi] = vec![-1.0; n];
    // Faint beat lines for a sense of speed.
    let neck = fretboard::fret_pos(fretboard::NECK_FRETS as f32);
    let (pos, scale) = match view {
        View::D3 => (
            Vec3::new(0.0, 0.005, 0.0),
            Vec3::new(5.0 * gfx::LANE3 + 0.5, 0.01, 0.02),
        ),
        View::D2 => (
            Vec3::new(gfx::HIT_X, 0.0, 0.005),
            Vec3::new(0.02, 5.0 * gfx::LANE2 + 0.5, 0.01),
        ),
        View::Fret3 => (
            Vec3::new(neck / 2.0, 0.005, 0.0),
            Vec3::new(neck, 0.008, 0.02),
        ),
    };
    let beats = (chart.length / chart.beat) as i32 + 4;
    for k in -4..=beats {
        let strong = k.rem_euclid(4) == 0;
        let mat = if strong { &vm.beat_strong } else { &vm.beat };
        let mut m = Ctx::mover(k as f32 * chart.beat, pos, scale);
        m.fade = false;
        cx.part(&shapes.cube, mat, m, Quat::IDENTITY);
    }
}

/// Per-string receptors on the hit plane (static, survive chart switches).
/// They are hollow rings and small marks, never filled discs, so they cannot be read as notes.
pub fn spawn_indicators(commands: &mut Commands, view: View, shapes: &Shapes, mats: &Mats) {
    let vm = mats.of(view);
    // Lanes view: receptors lie flat on the floor under the gems.
    let flat = if view == View::D3 {
        Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)
    } else {
        Quat::IDENTITY
    };
    for s in 0..6usize {
        let mut pos = gfx::pt(view, s, 1);
        match view {
            View::D3 => pos.y = 0.02,
            View::D2 => pos.z = 0.02,
            View::Fret3 => {
                // On the string, just inside the left edge of the hand zone.
                pos.x = fretboard::fret_pos(0.0) + 0.14;
                pos.z = 0.02;
            }
        }
        let sc = if view == View::Fret3 { 0.4 } else { 1.0 };
        let mut put = |mesh: &Handle<Mesh>, mat: &Mat, kind, base: Vec3, rot: Quat| {
            commands.spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat.clone()),
                Transform {
                    translation: pos,
                    rotation: flat * rot,
                    scale: base * sc,
                },
                Indicator {
                    string: s,
                    kind,
                    base: base * sc,
                },
                InView(view),
                Visibility::Hidden,
                view.layer(),
            ));
        };
        match view {
            // One dim ring per lane, centred on the lane's gem path at the hit line.
            View::D3 => put(
                &shapes.ring,
                &vm.idle,
                IndicatorKind::Idle,
                Vec3::splat(0.48),
                Quat::IDENTITY,
            ),
            View::D2 => put(
                &shapes.disc,
                &vm.idle,
                IndicatorKind::Idle,
                Vec3::splat(0.08),
                Quat::IDENTITY,
            ),
            View::Fret3 => {}
        }
        put(
            &shapes.ring,
            &vm.rang,
            IndicatorKind::Rang,
            Vec3::splat(0.48),
            Quat::IDENTITY,
        );
        put(
            &shapes.ring,
            &vm.missing,
            IndicatorKind::Missing,
            Vec3::splat(0.3),
            Quat::IDENTITY,
        );
        for a in [45f32, -45.0] {
            put(
                &shapes.cube,
                &vm.extra,
                IndicatorKind::Extra,
                Vec3::new(0.2, 0.045, 0.04),
                Quat::from_rotation_z(a.to_radians()),
            );
        }
    }
}
