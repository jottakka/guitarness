//! What the judge's answers look like: marks per stroke, one return per attack.
//!
//! The logic here is pure (no entities): [`PulseFeedback::apply`] turns a [`JudgeOut`] into a
//! mark. Nothing here is an accusation. "Not heard", "not judged" (glitch, no calibration,
//! unlocked clock, withdrawn) and "contested" are shapes in white or grey, and no number is
//! ever drawn: the shift of the arrow is a distance, not a reading.

use bevy::prelude::*;
use tonica_core::contract::{Aspect, EventContract};
use tonica_core::verdict::{Attack, JudgeOut, PulseVerdict, Timing, Why};

/// How a heard stroke sat against the click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// Centred, glows. Only when the judge says `Center` (a measured loop).
    Center,
    InWindow,
    /// Heard, with no timing to show (outside the window, or not timed): plain, no glow.
    Plain,
}

/// The state of one expected stroke on screen. There is no failure variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MarkKind {
    /// Not decided yet (or not reached).
    #[default]
    Waiting,
    Heard(Quality),
    /// Hollow grey ring.
    NotHeard,
    /// Faded, with a lock.
    NotJudged,
    /// A ghost stroke: nothing was expected to sound.
    Ghost,
    /// Contested: a neutral dot.
    Contested,
}

impl MarkKind {
    pub const ALL: [MarkKind; 8] = [
        MarkKind::Waiting,
        MarkKind::Heard(Quality::Center),
        MarkKind::Heard(Quality::InWindow),
        MarkKind::Heard(Quality::Plain),
        MarkKind::NotHeard,
        MarkKind::NotJudged,
        MarkKind::Ghost,
        MarkKind::Contested,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EventMark {
    pub kind: MarkKind,
    /// The arrow was filled by an attack.
    pub flashed: bool,
    /// Song time of the attack's flash (for a short pop).
    pub flash_t: f32,
    /// Signed shift of the arrow along the time axis, -1..=1 (negative = early), capped at
    /// the window. `None`: no timing to show.
    pub shift: Option<f32>,
}

/// One return per attack heard, in order of arrival.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlashRecord {
    pub id: Option<u32>,
    /// What the judge measured, ms (negative = early). Never drawn.
    pub dt_ms: Option<f32>,
    pub shift: Option<f32>,
    pub t: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stray {
    pub t: f32,
}

/// The marks of the round on screen.
#[derive(Resource, Default, Debug)]
pub struct PulseFeedback {
    pub events: Vec<EventMark>,
    /// Attack with no stroke to belong to: a small arrow at the hit line.
    pub strays: Vec<Stray>,
    /// Extra strokes: a small white dash.
    pub extras: Vec<Stray>,
    pub flashes: Vec<FlashRecord>,
    /// Tolerance window of the level in use, ms: the cap of the shift.
    pub window_ms: f32,
}

/// Distance of the arrow from its ring, as a share of the maximum: proportional to the
/// timing error, capped at the window.
pub fn shift_of(dt_ms: f32, window_ms: f32) -> f32 {
    if window_ms <= 0.0 || !dt_ms.is_finite() {
        return 0.0;
    }
    (dt_ms / window_ms).clamp(-1.0, 1.0)
}

fn kind_of(v: &PulseVerdict) -> MarkKind {
    match v.attack {
        Attack::NotHeard => MarkKind::NotHeard,
        Attack::NotJudged(Why::Ghost) => MarkKind::Ghost,
        Attack::NotJudged(Why::Disputed) => MarkKind::Contested,
        Attack::NotJudged(_) => MarkKind::NotJudged,
        Attack::Heard => MarkKind::Heard(match v.timing {
            Timing::Center => Quality::Center,
            Timing::InWindow => Quality::InWindow,
            Timing::Outside | Timing::NotJudged(_) => Quality::Plain,
        }),
    }
}

impl PulseFeedback {
    pub fn new(n_events: usize, window_ms: f32) -> Self {
        PulseFeedback {
            events: vec![EventMark::default(); n_events],
            window_ms,
            ..Default::default()
        }
    }

    pub fn reset(&mut self, n_events: usize, window_ms: f32) {
        *self = PulseFeedback::new(n_events, window_ms);
    }

    /// Feeds one answer of the judge; `now` is the song time on arrival. Returns true for
    /// an attack heard (the caller counts it).
    pub fn apply(&mut self, out: JudgeOut, now: f32) -> bool {
        match out {
            JudgeOut::Flash { id, dt_ms } => {
                let shift = dt_ms.map(|d| shift_of(d, self.window_ms));
                self.flashes.push(FlashRecord {
                    id,
                    dt_ms,
                    shift,
                    t: now,
                });
                match id.and_then(|i| self.events.get_mut(i as usize)) {
                    Some(m) => {
                        m.flashed = true;
                        m.flash_t = now;
                        m.shift = shift;
                    }
                    None => self.strays.push(Stray { t: now }),
                }
                true
            }
            JudgeOut::Verdict(v) => {
                if let Some(m) = self.events.get_mut(v.id as usize) {
                    m.kind = kind_of(&v);
                    if let Some(d) = v.dt_ms {
                        m.shift = Some(shift_of(d, self.window_ms));
                    }
                    if matches!(m.kind, MarkKind::NotHeard | MarkKind::NotJudged) {
                        m.flashed = false;
                        m.shift = None;
                    }
                }
                false
            }
            JudgeOut::Withdrawn { id, why } => {
                // A withdrawn verdict says nothing about the player: neutral, not a miss.
                if let Some(m) = self.events.get_mut(id as usize) {
                    m.kind = if why == Why::Disputed {
                        MarkKind::Contested
                    } else {
                        MarkKind::NotJudged
                    };
                    m.flashed = false;
                    m.shift = None;
                }
                false
            }
            JudgeOut::Extra { .. } => {
                self.extras.push(Stray { t: now });
                false
            }
        }
    }

    /// Mean of the timing errors the judge measured, ms (tests and tooling; never drawn).
    pub fn mean_dt_ms(&self) -> Option<f64> {
        let v: Vec<f64> = self
            .flashes
            .iter()
            .filter_map(|f| f.dt_ms.map(f64::from))
            .collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    }
}

/// What the screen may show of a mark, given the event's contract (plan W2). An event whose
/// attack is only a guide (not judgeable: no calibration, locked gate) shows no result at all,
/// whatever the judge sent: it stays an instruction. A timing that is only a guide shows the
/// stroke as heard, with no centre and no shift. Without a contract, or for an event that asks
/// nothing of the attack (a ghost), the mark is shown as it is.
pub fn shown(mark: &EventMark, c: Option<&EventContract>) -> EventMark {
    let Some(c) = c else { return *mark };
    if !c.requested(Aspect::Attack) {
        return *mark;
    }
    if !c.judged(Aspect::Attack) {
        return EventMark::default();
    }
    let mut m = *mark;
    if !c.judged(Aspect::Timing) {
        m.shift = None;
        if matches!(m.kind, MarkKind::Heard(_)) {
            m.kind = MarkKind::Heard(Quality::Plain);
        }
    }
    m
}

// ------------------------------------------------------------------ drawing the marks

use super::assets::{MAX_SHIFT, POOL, Part, PartKind, PoolMark, PulseEv, PulseGrid, PulseView};
use super::flow::{Phase, PulseFlow, PulseRound};
use crate::contract::RoundContracts;
use crate::gfx::SongClock;

/// Where, in the local frame of an event root, the time axis points (toward the future).
pub fn future_local(view: PulseView) -> Vec3 {
    match view {
        // The root lies flat (rotated -90 degrees about X): local +Y is far down the lane.
        PulseView::D3 => Vec3::Y,
        PulseView::D2 => Vec3::X,
    }
}

fn want_visible(kind: PartKind, mark: &EventMark, direction_asked: bool) -> bool {
    let k = mark.kind;
    match kind {
        PartKind::Ring => !matches!(
            k,
            MarkKind::NotHeard | MarkKind::NotJudged | MarkKind::Ghost | MarkKind::Contested
        ),
        PartKind::Ring2 => false,
        PartKind::Guide => direction_asked && matches!(k, MarkKind::Waiting | MarkKind::Ghost),
        PartKind::Fill => mark.flashed && !matches!(k, MarkKind::NotHeard | MarkKind::NotJudged),
        PartKind::NotHeard => k == MarkKind::NotHeard,
        PartKind::Lock => k == MarkKind::NotJudged,
        PartKind::Dim => matches!(
            k,
            MarkKind::NotJudged | MarkKind::Ghost | MarkKind::Contested
        ),
        PartKind::Center => k == MarkKind::Heard(Quality::Center),
        PartKind::Dispute => k == MarkKind::Contested,
    }
}

/// Shows and shapes the parts of every stroke from the marks.
pub fn apply_marks(
    fb: Res<PulseFeedback>,
    clock: Res<SongClock>,
    round: Res<PulseRound>,
    contracts: Option<Res<RoundContracts>>,
    mut parts: Query<(&Part, &mut Visibility, &mut Transform)>,
) {
    for (p, mut vis, mut tf) in &mut parts {
        let Some(raw) = fb.events.get(p.id) else {
            continue;
        };
        // The contract decides what may be shown: a guide never gets a result mark.
        let c = contracts.as_deref().and_then(|t| t.get(p.id));
        let shown_mark = shown(raw, c);
        let mark = &shown_mark;
        let mut show = want_visible(p.kind, mark, crate::contract::asks_direction(c));
        // The inner ring marks the downbeat.
        if p.kind == PartKind::Ring2 {
            show = round.strong.get(p.id).copied().unwrap_or(false)
                && !matches!(mark.kind, MarkKind::NotHeard | MarkKind::NotJudged);
        }
        let want = if show {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
        if p.kind == PartKind::Fill && show {
            let dir = future_local(p.view);
            let shift = mark.shift.unwrap_or(0.0);
            let z = tf.translation.z;
            tf.translation = dir * shift * MAX_SHIFT;
            tf.translation.z = z;
            // A short pop when the arrow fills.
            let age = (clock.t - mark.flash_t).max(0.0);
            let pop = 1.0 + 0.35 * (-age * 10.0).exp();
            let target = round.fill_scale(p.id) * pop;
            tf.scale = Vec3::new(target, target, 1.0);
        }
    }
}

/// Places strokes and beat lines along the time axis, and hides what is far or long gone.
pub fn place_events(
    clock: Res<SongClock>,
    flow: Res<PulseFlow>,
    round: Res<PulseRound>,
    mut evs: Query<(&PulseEv, &mut Transform, &mut Visibility), Without<PulseGrid>>,
    mut grid: Query<(&PulseGrid, &mut Transform, &mut Visibility), Without<PulseEv>>,
) {
    let now = clock.t;
    let show = flow.screen_on && matches!(flow.phase, Phase::Counting | Phase::Playing);
    for (e, mut tf, mut vis) in &mut evs {
        let (pos, on) = super::track(e.view, e.t - now);
        tf.translation = pos;
        if e.view == PulseView::D2 {
            // Smaller rings with room between them, the downbeat no bigger than the rest
            // (it keeps its second ring), and the one at the playhead a little larger.
            let strong = round.strong.get(e.id).copied().unwrap_or(false);
            let base = if strong { 0.62 } else { 0.8 };
            let dt = e.t - now;
            let here = (-(dt / 0.35) * (dt / 0.35)).exp();
            tf.scale = Vec3::splat(base * (1.0 + 0.25 * here));
        }
        let want = if on && show {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }
    for (g, mut tf, mut vis) in &mut grid {
        let (pos, on) = super::track(g.view, g.t - now);
        tf.translation = pos;
        if g.view == PulseView::D2 {
            // Behind the strokes, so a glyph's mask can hide the tick under it.
            tf.translation.z = -0.02;
        }
        let want = if on && show {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }
}

/// Stray arrows and extra dashes: small marks at the hit line that scroll away.
pub fn place_pool(
    clock: Res<SongClock>,
    fb: Res<PulseFeedback>,
    flow: Res<PulseFlow>,
    mut q: Query<(&PoolMark, &mut Transform, &mut Visibility), Without<PulseEv>>,
) {
    // Like the strokes: nothing is left on the lane once the round is over.
    let live = flow.screen_on && matches!(flow.phase, Phase::Counting | Phase::Playing);
    for (p, mut tf, mut vis) in &mut q {
        let list = if p.extra { &fb.extras } else { &fb.strays };
        // The newest POOL marks live in slots by arrival order.
        let n = list.len();
        let slot_item = (n > p.slot).then(|| {
            // Slot s holds mark number m where m % POOL == s, the newest such.
            let last = n - 1;
            let m = last - ((last + POOL - p.slot) % POOL);
            list[m]
        });
        let want = match slot_item {
            Some(s) if live && clock.t - s.t < 1.6 && clock.t >= s.t - 0.01 => {
                let (pos, _) = super::track(p.view, s.t - clock.t);
                tf.translation = pos
                    + if p.extra {
                        // The dash sits beside the line, never on a ring.
                        super::extra_offset(p.view)
                    } else {
                        Vec3::ZERO
                    };
                Visibility::Inherited
            }
            _ => Visibility::Hidden,
        };
        if *vis != want {
            *vis = want;
        }
    }
}
