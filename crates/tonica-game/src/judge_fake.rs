//! Deterministic fake judge: no audio, verdicts derived from event order.
//!
//! Rules: every 4th pitched strum has an extra string; every 5th has string 4 silent;
//! every 2nd partial strum has an extra neighbour string; every 2nd chuck is missed;
//! ghost strokes are never judged. Error marks use white/gray and shape only, never hue.

use bevy::prelude::*;

use crate::chart::{Chart, Kind, Style};
use crate::fretboard::{self, Zone};
use crate::gfx::{
    ActiveView, FlashMats, HitLine, InView, Indicator, IndicatorKind, SongClock, View,
};
use crate::palette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Rang,
    Missing,
    Extra,
}

pub const SHOW_FOR: f32 = 0.55;

#[derive(Resource)]
pub struct FakeJudge {
    pub verdicts: Vec<[Option<Verdict>; 6]>,
}

impl FakeJudge {
    pub fn new(chart: &Chart) -> Self {
        let (mut strums, mut partials, mut mutes) = (0usize, 0usize, 0usize);
        let verdicts = chart
            .events
            .iter()
            .map(|ev| {
                let mut v = [None; 6];
                if !Chart::judged(ev) {
                    return v;
                }
                let expected = chart.expected(ev);
                for s in 0..6 {
                    if expected[s] {
                        v[s] = Some(Verdict::Rang);
                    }
                }
                match ev.kind {
                    Kind::Strum {
                        style: Style::Mute, ..
                    } => {
                        mutes += 1;
                        if mutes.is_multiple_of(2) {
                            let covered = chart.covered(ev);
                            for s in 0..6 {
                                if covered[s] {
                                    v[s] = Some(Verdict::Missing);
                                }
                            }
                        }
                    }
                    Kind::Strum {
                        style: st @ (Style::Bass | Style::Treble),
                        ..
                    } => {
                        partials += 1;
                        if partials.is_multiple_of(2) {
                            let s = if st == Style::Bass { 3 } else { 2 };
                            v[s] = Some(Verdict::Extra);
                        }
                    }
                    Kind::Strum { .. } => {
                        strums += 1;
                        if strums.is_multiple_of(4)
                            && let Some(s) = (0..6).find(|s| !expected[*s])
                        {
                            v[s] = Some(Verdict::Extra);
                        }
                        if strums.is_multiple_of(5) && expected[4] {
                            v[4] = Some(Verdict::Missing);
                        }
                    }
                    Kind::Note { .. } => {}
                }
                v
            })
            .collect();
        Self { verdicts }
    }

    /// Latest judged event already crossed and its age in seconds.
    pub fn latest(&self, chart: &Chart, now: f32) -> Option<(usize, f32)> {
        let i = chart
            .events
            .iter()
            .rposition(|e| e.t <= now && Chart::judged(e))?;
        Some((i, now - chart.events[i].t))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn update_indicators(
    clock: Res<SongClock>,
    chart: Res<Chart>,
    judge: Res<FakeJudge>,
    zone: Res<Zone>,
    active: Res<ActiveView>,
    mut q: Query<(&Indicator, &InView, &mut Visibility, &mut Transform)>,
) {
    let latest = judge
        .latest(&chart, clock.t)
        .filter(|(_, age)| *age < SHOW_FOR);
    for (ind, view, mut vis, mut tf) in &mut q {
        if view.0 != active.0 {
            continue;
        }
        let verdict = latest.and_then(|(i, _)| judge.verdicts[i][ind.string]);
        let age = latest.map_or(1.0, |(_, a)| a / SHOW_FOR);
        let show = match ind.kind {
            IndicatorKind::Idle => verdict.is_none(),
            IndicatorKind::Rang => verdict == Some(Verdict::Rang),
            IndicatorKind::Missing => verdict == Some(Verdict::Missing),
            IndicatorKind::Extra => verdict == Some(Verdict::Extra),
        };
        *vis = if show {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        let pulse = if ind.kind == IndicatorKind::Rang {
            1.0 + 0.25 * (1.0 - age).max(0.0)
        } else {
            1.0
        };
        tf.scale = ind.base * pulse;
        if view.0 == View::Fret3 {
            tf.translation.x = fretboard::fret_pos(zone.lo - 1.0) + 0.14;
        }
    }
}

pub fn update_flash(
    clock: Res<SongClock>,
    chart: Res<Chart>,
    judge: Res<FakeJudge>,
    flash: Res<FlashMats>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut q: Query<(&HitLine, &mut Transform)>,
) {
    let age = judge.latest(&chart, clock.t).map_or(9.0, |(_, a)| a);
    let f = (1.0 - age / 0.22).clamp(0.0, 1.0);
    let k = 0.8 + 2.5 * f;
    let c = palette::HIT.to_linear();
    let lin = LinearRgba::new(c.red * k, c.green * k, c.blue * k, 1.0);
    if let Some(mut m) = mats.get_mut(&flash.m3) {
        m.emissive = lin;
    }
    if let Some(mut m) = mats.get_mut(&flash.mf) {
        m.emissive = lin;
    }
    if let Some(mut m) = mats.get_mut(&flash.m2) {
        m.base_color = Color::LinearRgba(lin);
    }
    for (line, mut tf) in &mut q {
        tf.scale = line.base * (Vec3::ONE + line.thick * 2.0 * f);
    }
}
