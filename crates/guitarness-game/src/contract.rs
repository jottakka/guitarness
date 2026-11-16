//! The per-event contract in the game (plan W2, K2): "asked" versus "judgeable".
//!
//! What an event asks of the player comes from the chart. What the microphone can verify is
//! derived live from the gate table and the calibration, never stored. An aspect that is asked
//! but not judgeable is a *guide*: it is shown like an instruction (neutral grey, an outline or
//! a plain shape) and never carries a result mark. Instructions never imply verification.
//!
//! * [`ContractGates`]: the gate table and the calibration the contract is derived from.
//! * [`RoundContracts`]: one [`EventContract`] per stroke of the round on screen, in the
//!   judge's id order; mirrored on each event root as an [`EventContractOf`] component.
//! * [`GuideGlyph`]: the small shapes of the guide-only aspects (muted, accent, strings). The
//!   direction guide is the arrow the Pulse views already draw.
//!
//! Chord feedback (V4/V2b rules: accuse at chord level only, never a "neighbour" kind, a
//! detuned guitar gets no right/wrong, a failed tuning check gives a generic retry) has no
//! result mark here: chord, string and fret aspects are guide-only for now.

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use guitarness_core::chart::{Chart, EventKind, Loc};
use guitarness_core::contract::{Aspect, Check, EventContract, contract};
use guitarness_core::gates::GateTable;

use crate::AppConfig;
use crate::pulse::assets::{PulseEv, PulseShapes, PulseView};
use crate::pulse::feedback::{MarkKind, PulseFeedback, shown};
use crate::pulse::flow::{PulseFlow, PulseRound, build_chart};

/// Gate state the contract is derived from (K1 `GateTable` plus calibration).
#[derive(Resource, Clone, Debug)]
pub struct ContractGates {
    pub gates: GateTable,
    /// The clock is calibrated, so attacks can be placed against the click. Follows the
    /// soundcheck's basis when that resource exists; without a calibration the rhythm is
    /// shown, not judged.
    pub timing_judged: bool,
}

impl Default for ContractGates {
    fn default() -> Self {
        ContractGates {
            gates: GateTable::builtin(),
            timing_judged: false,
        }
    }
}

/// The contract of one event, on its root entity (mirror of [`RoundContracts`]).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventContractOf(pub EventContract);

/// Contracts of the round on screen, by judge id (events that are not rests, in order).
#[derive(Resource, Clone, Debug, Default)]
pub struct RoundContracts {
    /// The `PulseRound::rev` they were derived for.
    pub rev: u32,
    pub per: Vec<EventContract>,
}

impl RoundContracts {
    pub fn get(&self, id: usize) -> Option<&EventContract> {
        self.per.get(id)
    }
}

/// The contracts of every event of the chart's first tier that is not a rest, in judge-id
/// order (the same order as `pulse::expectations`).
pub fn contracts_of(chart: &Chart, g: &ContractGates) -> Vec<EventContract> {
    let Some(tier) = chart.tiers.first() else {
        return Vec::new();
    };
    tier.events
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e.kind, EventKind::Rest))
        .map(|(index, _)| contract(chart, Loc { tier: 0, index }, &g.gates, g.timing_judged))
        .collect()
}

/// What a guide shows for an aspect asked but not judgeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GuideKind {
    /// The stroke direction: the arrow in the ring.
    Direction,
    /// "Dampen the strings": a small cross.
    Muted,
    /// "Hit this one harder": a small diamond.
    Accent,
    /// "Chord, strings, fret": three short bars (one glyph for the three aspects).
    Strings,
}

impl GuideKind {
    /// The glyphs drawn as separate shapes (the direction arrow is part of the ring).
    pub const GLYPHS: [GuideKind; 3] = [GuideKind::Muted, GuideKind::Accent, GuideKind::Strings];
}

/// Guides of a contract: every aspect asked and not judgeable, one entry per glyph.
pub fn guides_of(c: &EventContract) -> Vec<GuideKind> {
    let mut out = Vec::new();
    let mut push = |k: GuideKind| {
        if !out.contains(&k) {
            out.push(k);
        }
    };
    for a in Aspect::ALL {
        if !matches!(c.get(a), Some(Check::Guide(_))) {
            continue;
        }
        match a {
            Aspect::Direction => push(GuideKind::Direction),
            Aspect::Muted => push(GuideKind::Muted),
            Aspect::Accent => push(GuideKind::Accent),
            Aspect::Chord | Aspect::Strings | Aspect::Fret => push(GuideKind::Strings),
            // The attack and its timing are the ring itself: a guide ring has no glyph.
            Aspect::Attack | Aspect::Timing => {}
        }
    }
    out
}

/// True when this event asks for a stroke direction (the arrow is an instruction either way).
pub fn asks_direction(c: Option<&EventContract>) -> bool {
    c.is_none_or(|c| c.requested(Aspect::Direction))
}

/// A glyph of a guide-only aspect, child of an event root.
#[derive(Component, Clone, Copy, Debug)]
pub struct GuideGlyph {
    pub id: usize,
    pub kind: GuideKind,
    pub view: PulseView,
}

/// Neutral materials of the guides: grey (saturation 0), never a verdict colour.
#[derive(Resource, Clone)]
pub struct GuideMats {
    pub glyph: Handle<StandardMaterial>,
}

/// Grey level and glow of a guide: quieter than a ring waiting for its stroke.
pub const GUIDE_GREY: (f32, f32) = (0.5, 0.9);

pub fn guide_material() -> StandardMaterial {
    let (g, k) = GUIDE_GREY;
    StandardMaterial {
        base_color: Color::linear_rgb(g * k, g * k, g * k),
        unlit: true,
        ..default()
    }
}

fn build_guide_mats(mut commands: Commands, mats: Option<ResMut<Assets<StandardMaterial>>>) {
    if let Some(mut mats) = mats {
        commands.insert_resource(GuideMats {
            glyph: mats.add(guide_material()),
        });
    }
}

/// The calibration follows the soundcheck's basis when there is one.
fn sync_gates(sound: Option<Res<crate::soundcheck::Soundcheck>>, mut g: ResMut<ContractGates>) {
    if let Some(s) = sound {
        let j = s.judged();
        if g.timing_judged != j {
            g.timing_judged = j;
        }
    }
}

/// Derives the contracts again when a new round is set or the gates change.
fn build_contracts(
    flow: Option<Res<PulseFlow>>,
    round: Option<Res<PulseRound>>,
    gates: Res<ContractGates>,
    mut table: ResMut<RoundContracts>,
) {
    // Without the Pulse screen there is no round to derive contracts for.
    let (Some(flow), Some(round)) = (flow, round) else {
        return;
    };
    if table.rev == round.rev && !gates.is_changed() {
        return;
    }
    table.rev = round.rev;
    table.per = if round.rev == 0 {
        Vec::new()
    } else {
        contracts_of(&build_chart(&flow), &gates)
    };
}

/// Local offset of a glyph from the ring's centre, in units of the ring size: beside the ring
/// on the lane (3D) and below it on the time line (2D), clear of the neighbouring strokes.
pub fn glyph_offset(view: PulseView) -> Vec3 {
    match view {
        PulseView::D3 => Vec3::new(0.72, 0.0, 0.0),
        PulseView::D2 => Vec3::new(0.0, -0.78, 0.0),
    }
}

/// Gives each event root its contract component and its (hidden) guide glyphs.
fn attach_contracts(
    mut commands: Commands,
    table: Res<RoundContracts>,
    shapes: Option<Res<PulseShapes>>,
    mats: Option<Res<GuideMats>>,
    evs: Query<(Entity, &PulseEv, Option<&EventContractOf>)>,
) {
    for (e, ev, have) in &evs {
        let want = EventContractOf(table.get(ev.id).copied().unwrap_or_default());
        if have.is_some() {
            if have != Some(&want) {
                commands.entity(e).insert(want);
            }
            continue;
        }
        commands.entity(e).insert(want);
        let (Some(shapes), Some(mats)) = (&shapes, &mats) else {
            continue;
        };
        let layer = ev.view.layer();
        for kind in GuideKind::GLYPHS {
            commands.entity(e).with_children(|c| {
                spawn_glyph(c, shapes, mats, ev, kind, layer.clone());
            });
        }
    }
}

fn spawn_glyph(
    c: &mut ChildSpawnerCommands,
    shapes: &PulseShapes,
    mats: &GuideMats,
    ev: &PulseEv,
    kind: GuideKind,
    layer: RenderLayers,
) {
    // Placed and sized by the ring it belongs to in `fit_glyphs`.
    let bar = |s: Vec3, p: Vec3, rot: f32| {
        (
            Mesh3d(shapes.bar.clone()),
            MeshMaterial3d(mats.glyph.clone()),
            Transform::from_translation(p)
                .with_rotation(Quat::from_rotation_z(rot))
                .with_scale(s),
            layer.clone(),
        )
    };
    c.spawn((
        Transform::default(),
        Visibility::Hidden,
        GuideGlyph {
            id: ev.id,
            kind,
            view: ev.view,
        },
        layer.clone(),
    ))
    .with_children(|g| match kind {
        GuideKind::Muted => {
            let a = std::f32::consts::FRAC_PI_4;
            g.spawn(bar(Vec3::new(0.34, 0.06, 1.0), Vec3::ZERO, a));
            g.spawn(bar(Vec3::new(0.34, 0.06, 1.0), Vec3::ZERO, -a));
        }
        GuideKind::Accent => {
            g.spawn((
                Mesh3d(shapes.diamond.clone()),
                MeshMaterial3d(mats.glyph.clone()),
                Transform::from_scale(Vec3::splat(0.3)),
                layer.clone(),
            ));
        }
        GuideKind::Strings => {
            for k in -1..=1 {
                g.spawn(bar(
                    Vec3::new(0.36, 0.045, 1.0),
                    Vec3::new(0.0, 0.09 * k as f32, 0.0),
                    0.0,
                ));
            }
        }
        GuideKind::Direction => {}
    });
}

/// Scales and places each glyph by the ring it belongs to (rings differ in size).
fn fit_glyphs(round: Option<Res<PulseRound>>, mut q: Query<(&GuideGlyph, &mut Transform)>) {
    let Some(round) = round else { return };
    for (g, mut tf) in &mut q {
        let ring = round.sizes.get(g.id).copied().unwrap_or(1.0);
        let want = glyph_offset(g.view) * ring + Vec3::new(0.0, 0.0, 0.002);
        if tf.translation != want {
            tf.translation = want;
        }
        let s = Vec3::splat(ring * 0.9);
        if tf.scale != s {
            tf.scale = s;
        }
    }
}

/// Shows a glyph while its stroke is an instruction (before any result) and its aspect is a
/// guide. Never a result mark: no glyph outlives the stroke's turn on the lane as a verdict.
fn show_guides(
    table: Res<RoundContracts>,
    fb: Option<Res<PulseFeedback>>,
    mut q: Query<(&GuideGlyph, &mut Visibility)>,
) {
    let Some(fb) = fb else { return };
    for (g, mut vis) in &mut q {
        let c = table.get(g.id);
        let mark = fb.events.get(g.id).copied().unwrap_or_default();
        let m = shown(&mark, c);
        let waiting = matches!(m.kind, MarkKind::Waiting | MarkKind::Ghost);
        let on = waiting && c.is_some_and(|c| guides_of(c).contains(&g.kind));
        let want = if on {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }
}

/// The direction arrow of the ring is a guide too: neutral grey, not the old blue.
fn style_guide_arrows(
    mut commands: Commands,
    mats: Option<Res<GuideMats>>,
    parts: Query<(Entity, &crate::pulse::assets::Part), Added<crate::pulse::assets::Part>>,
) {
    let Some(mats) = mats else { return };
    for (e, p) in &parts {
        if p.kind == crate::pulse::assets::PartKind::Guide {
            commands
                .entity(e)
                .insert(MeshMaterial3d(mats.glyph.clone()));
        }
    }
}

pub struct ContractLogicPlugin;

impl Plugin for ContractLogicPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ContractGates>()
            .init_resource::<RoundContracts>()
            .add_systems(Startup, build_guide_mats)
            .add_systems(
                Update,
                (
                    sync_gates,
                    build_contracts.after(crate::pulse::flow::handle_actions),
                    attach_contracts
                        .after(build_contracts)
                        .after(crate::pulse::view3d::respawn_round)
                        .after(crate::pulse::view2d::respawn_round),
                    fit_glyphs.after(attach_contracts),
                    style_guide_arrows.after(attach_contracts),
                    show_guides
                        .after(attach_contracts)
                        .before(crate::pulse::feedback::apply_marks),
                ),
            );
    }
}

pub fn install(app: &mut App, _cfg: &AppConfig) {
    app.add_plugins(ContractLogicPlugin);
}
