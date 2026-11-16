//! W2: the chart contract in the Pulse views ("asked" versus "judgeable"). No window, no
//! device: a headless app with the real systems and meshes that are never rendered.

use bevy::prelude::*;
use guitarness_core::chart::{Chart, Dir, Event, EventKind, Style};
use guitarness_core::contract::{Aspect, Check, EventContract};
use guitarness_core::gates::{ALL_TYPES, GateEntry, GateEvidence, GateRule, GateState, GateTable};
use guitarness_core::pulse::{Exercise, exercise};
use guitarness_core::verdict::{Attack, Direction, JudgeOut, PulseVerdict, Timing, Why};
use guitarness_game::contract::{
    ContractGates, ContractLogicPlugin, GuideGlyph, GuideKind, GuideMats, RoundContracts,
    contracts_of, guide_material, guides_of,
};
use guitarness_game::gfx::SongClock;
use guitarness_game::pulse::assets::{
    Part, PartKind, PulseMats, build_mats, build_shapes, material_colors, saturation,
};
use guitarness_game::pulse::feedback::{EventMark, MarkKind, PulseFeedback, apply_marks, shown};
use guitarness_game::pulse::flow::{PulseFlow, PulseRound, build_chart, build_round};
use guitarness_game::pulse::view3d;

fn gates_all(state: GateState) -> GateTable {
    let mut t = GateTable::builtin();
    for ty in ALL_TYPES {
        t.set(
            ty,
            GateEntry {
                state,
                rule: GateRule::default(),
                evidence: GateEvidence::default(),
                reason: "test".into(),
            },
        );
    }
    t
}

fn mixed_chart() -> Chart {
    let mut c = Chart::new("mixed", 90.0);
    let kinds = [
        EventKind::Strum {
            dir: Dir::Down,
            span: None,
            style: Style::Full,
            roll_ms: None,
        },
        EventKind::Strum {
            dir: Dir::Up,
            span: None,
            style: Style::Accent,
            roll_ms: None,
        },
        EventKind::Strum {
            dir: Dir::Down,
            span: None,
            style: Style::Pm,
            roll_ms: None,
        },
        EventKind::Mute { dir: Dir::Down },
        EventKind::Ghost { dir: Dir::Up },
        EventKind::Note {
            string: 2,
            fret: 3,
            finger: Some('i'),
        },
        EventKind::Rest,
    ];
    for (i, kind) in kinds.into_iter().enumerate() {
        c.tiers[0].events.push(Event {
            tick: i as u32 * 480,
            dur: 480,
            kind,
            chord: Some("C".into()),
        });
    }
    c
}

fn all_gates() -> Vec<ContractGates> {
    let mut mixed = GateTable::builtin();
    mixed.set(
        guitarness_core::verdict::VerdictType::Muted,
        GateEntry {
            state: GateState::Open,
            rule: GateRule::default(),
            evidence: GateEvidence::default(),
            reason: "test".into(),
        },
    );
    let mut out = Vec::new();
    for gates in [
        GateTable::builtin(),
        gates_all(GateState::Locked),
        gates_all(GateState::Provisional),
        gates_all(GateState::Open),
        mixed,
    ] {
        for timing_judged in [false, true] {
            out.push(ContractGates {
                gates: gates.clone(),
                timing_judged,
            });
        }
    }
    out
}

fn verdict(id: u32, attack: Attack, timing: Timing, dt: Option<f32>) -> PulseVerdict {
    PulseVerdict {
        id,
        attack,
        timing,
        dt_ms: dt,
        dir: Direction::NotJudged(Why::Ghost),
        onset_in: None,
        pos_in_bar: 0,
        fin: true,
    }
}

/// Every mark kind the judge can produce, as it would arrive in a mark.
fn all_marks() -> Vec<EventMark> {
    MarkKind::ALL
        .iter()
        .map(|k| EventMark {
            kind: *k,
            flashed: true,
            flash_t: 1.0,
            shift: Some(0.4),
        })
        .collect()
}

// ---------------------------------------------------------------- contract derivation

#[test]
fn w2_1_pulse_asks_attack_and_timing_judged_rest_is_guide() {
    let chart = exercise(Exercise::OnClickEighths, 80.0, 2);
    let on = contracts_of(
        &chart,
        &ContractGates {
            timing_judged: true,
            ..Default::default()
        },
    );
    assert_eq!(on.len(), 16);
    for c in &on {
        assert!(c.judged(Aspect::Attack) && c.judged(Aspect::Timing));
        // Direction is never judged; a mute is asked but not verified by the gates.
        assert!(matches!(c.get(Aspect::Direction), Some(Check::Guide(_))));
        assert!(matches!(c.get(Aspect::Muted), Some(Check::Guide(_))));
        assert_eq!(guides_of(c), [GuideKind::Direction, GuideKind::Muted]);
    }
    // Derived live: no calibration, and nothing the microphone verifies is left.
    let off = contracts_of(&chart, &ContractGates::default());
    for c in &off {
        for a in Aspect::ALL {
            assert!(!c.judged(a), "{a:?} judged without a calibration");
        }
    }
}

#[test]
fn w2_2_judged_is_a_subset_of_asked_and_guides_never_get_a_mark() {
    let chart = mixed_chart();
    for g in all_gates() {
        let cs = contracts_of(&chart, &g);
        assert_eq!(cs.len(), 6, "the rest is not an event of the round");
        for c in &cs {
            for a in Aspect::ALL {
                if c.judged(a) {
                    assert!(c.requested(a));
                }
            }
            for m in all_marks() {
                let s = shown(&m, Some(c));
                if c.requested(Aspect::Attack) && !c.judged(Aspect::Attack) {
                    assert_eq!(s, EventMark::default(), "a guide event shows no result");
                }
                if c.judged(Aspect::Attack) && !c.judged(Aspect::Timing) {
                    assert_eq!(s.shift, None);
                    assert!(
                        !matches!(s.kind, MarkKind::Heard(q) if q != guitarness_game::pulse::feedback::Quality::Plain)
                    );
                }
            }
        }
    }
}

#[test]
fn w2_3_chord_aspects_are_guide_only() {
    let chart = mixed_chart();
    let cs = contracts_of(
        &chart,
        &ContractGates {
            timing_judged: true,
            ..Default::default()
        },
    );
    // The first event is a plain strum: with the shipped gates chord, strings and fret are
    // asked and none is verified, and they share one glyph.
    let c = &cs[0];
    for a in [Aspect::Chord, Aspect::Strings, Aspect::Fret] {
        assert!(matches!(c.get(a), Some(Check::Guide(_))), "{a:?}");
    }
    assert_eq!(
        guides_of(c),
        [GuideKind::Direction, GuideKind::Strings],
        "no verdict glyph for a chord"
    );
    // The ghost asks for a direction only, as a guide.
    assert_eq!(guides_of(&cs[4]), [GuideKind::Direction]);
}

// ---------------------------------------------------------------- the views

fn view_app(timing_judged: bool) -> App {
    let mut app = App::new();
    // Without the render plugins a mesh does not bring its visibility along.
    app.register_required_components::<Mesh3d, Visibility>();
    app.add_plugins(MinimalPlugins)
        .insert_resource(Assets::<Mesh>::default())
        .insert_resource(Assets::<StandardMaterial>::default())
        .insert_resource(SongClock {
            t: 0.0,
            running: true,
        })
        .insert_resource(ContractGates {
            timing_judged,
            ..Default::default()
        })
        .add_plugins(ContractLogicPlugin)
        .add_systems(Update, (view3d::respawn_round, apply_marks));
    let shapes = build_shapes(&mut app.world_mut().resource_mut::<Assets<Mesh>>());
    app.insert_resource(shapes);
    let mats: PulseMats =
        build_mats(&mut app.world_mut().resource_mut::<Assets<StandardMaterial>>());
    app.insert_resource(mats);
    app
}

/// Starts a round the way `Play` does, then feeds a verdict of every kind.
fn start_round(app: &mut App, judge: bool) {
    let flow = PulseFlow::default();
    let chart = build_chart(&flow);
    let mut round = build_round(&chart);
    round.rev = 1;
    let n = round.times.len();
    app.insert_resource(flow);
    app.insert_resource(round);
    let mut fb = PulseFeedback::new(n, 60.0);
    if judge {
        for i in 0..n as u32 {
            let (a, t, dt) = match i % 4 {
                0 => (Attack::Heard, Timing::Center, Some(2.0)),
                1 => (Attack::Heard, Timing::InWindow, Some(-40.0)),
                2 => (Attack::NotHeard, Timing::NotJudged(Why::NoAttack), None),
                _ => (
                    Attack::NotJudged(Why::Glitch),
                    Timing::NotJudged(Why::Glitch),
                    None,
                ),
            };
            if dt.is_some() {
                fb.apply(
                    JudgeOut::Flash {
                        id: Some(i),
                        dt_ms: dt,
                    },
                    0.5,
                );
            }
            fb.apply(JudgeOut::Verdict(verdict(i, a, t, dt)), 0.5);
        }
    }
    app.insert_resource(fb);
    for _ in 0..4 {
        app.update();
    }
}

fn visible_parts(app: &mut App, kinds: &[PartKind]) -> usize {
    let mut q = app.world_mut().query::<(&Part, &Visibility)>();
    q.iter(app.world())
        .filter(|(p, v)| kinds.contains(&p.kind) && **v == Visibility::Inherited)
        .count()
}

fn visible_glyphs(app: &mut App, kind: GuideKind) -> usize {
    let mut q = app.world_mut().query::<(&GuideGlyph, &Visibility)>();
    q.iter(app.world())
        .filter(|(g, v)| g.kind == kind && **v == Visibility::Inherited)
        .count()
}

const RESULT: [PartKind; 6] = [
    PartKind::Fill,
    PartKind::NotHeard,
    PartKind::Lock,
    PartKind::Dim,
    PartKind::Center,
    PartKind::Dispute,
];

#[test]
fn w2_4_no_calibration_no_result_mark_guides_stay() {
    let mut app = view_app(false);
    start_round(&mut app, true);
    let n = app.world().resource::<PulseRound>().times.len();
    assert!(n > 0);
    assert_eq!(
        visible_parts(&mut app, &RESULT),
        0,
        "a guide event has no result mark"
    );
    assert_eq!(visible_parts(&mut app, &[PartKind::Ring]), n);
    assert_eq!(visible_parts(&mut app, &[PartKind::Guide]), n);
    assert_eq!(visible_glyphs(&mut app, GuideKind::Muted), n);
    assert_eq!(visible_glyphs(&mut app, GuideKind::Accent), 0);
}

#[test]
fn w2_5_calibrated_marks_show_and_the_mute_glyph_waits_for_its_stroke() {
    let mut app = view_app(true);
    start_round(&mut app, true);
    assert!(visible_parts(&mut app, &RESULT) > 0);
    // Judged strokes are no longer instructions: no guide arrow, no glyph.
    assert_eq!(visible_parts(&mut app, &[PartKind::Guide]), 0);
    assert_eq!(visible_glyphs(&mut app, GuideKind::Muted), 0);

    // A fresh round, nothing heard yet: the instructions are on, mute included (asked, never
    // judged by the shipped gates).
    let mut app = view_app(true);
    start_round(&mut app, false);
    let n = app.world().resource::<PulseRound>().times.len();
    assert_eq!(visible_parts(&mut app, &RESULT), 0);
    assert_eq!(visible_parts(&mut app, &[PartKind::Guide]), n);
    assert_eq!(visible_glyphs(&mut app, GuideKind::Muted), n);
}

#[test]
fn w2_6_contract_is_derived_live() {
    let mut app = view_app(true);
    start_round(&mut app, true);
    assert!(visible_parts(&mut app, &RESULT) > 0);
    // The calibration goes away mid-round: the results leave the screen with it.
    app.world_mut()
        .resource_mut::<ContractGates>()
        .timing_judged = false;
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(visible_parts(&mut app, &RESULT), 0);
    let t = app.world().resource::<RoundContracts>();
    assert!(!t.per.iter().any(|c| c.judged(Aspect::Attack)));
    // Each event root carries its contract.
    let mut q = app
        .world_mut()
        .query::<&guitarness_game::contract::EventContractOf>();
    assert!(q.iter(app.world()).all(|c| !c.0.judged(Aspect::Attack)));
    assert!(q.iter(app.world()).count() > 0);
    let _ = EventContract::default();
}

#[test]
fn w2_7_guide_materials_are_grey() {
    for c in material_colors(&guide_material()) {
        assert_eq!(saturation(c), 0.0);
    }
    let mut app = view_app(true);
    start_round(&mut app, false);
    let glyph = app.world().resource::<GuideMats>().glyph.clone();
    let assets = app.world().resource::<Assets<StandardMaterial>>();
    for c in material_colors(assets.get(&glyph).unwrap()) {
        assert_eq!(saturation(c), 0.0);
    }
    // The direction arrow of every ring uses it too.
    let mut q = app
        .world_mut()
        .query::<(&Part, &MeshMaterial3d<StandardMaterial>)>();
    let arrows: Vec<_> = q
        .iter(app.world())
        .filter(|(p, _)| p.kind == PartKind::Guide)
        .map(|(_, m)| m.0.clone())
        .collect();
    assert!(!arrows.is_empty());
    assert!(arrows.iter().all(|h| *h == glyph));
}
