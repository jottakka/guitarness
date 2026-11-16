//! Requested versus judgeable contract (plan 2.5, unit K2). No device, no file.

use guitarness_core::chart::{Chart, Dir, Event, EventKind, Loc, Style};
use guitarness_core::contract::{Aspect, Check, EventContract, contract};
use guitarness_core::gates::{GateEntry, GateEvidence, GateRule, GateState, GateTable};
use guitarness_core::judge::{JudgeConfig, PulseJudge};
use guitarness_core::pulse::{Exercise, exercise};
use guitarness_core::stats::VerdictLog;
use guitarness_core::timeline::{demo_chart, gallery_chart};
use guitarness_core::timing::{Level, TimingRef};
use guitarness_core::verdict::{
    Attack, ExpectKind, Judge, JudgeInput, JudgeOut, Timing, VerdictType, Why,
};

fn table(state: Option<GateState>) -> GateTable {
    let mut g = GateTable::builtin();
    if let Some(s) = state {
        for t in guitarness_core::gates::ALL_TYPES {
            g.set(
                t,
                GateEntry {
                    state: s,
                    rule: GateRule::default(),
                    evidence: GateEvidence::default(),
                    reason: "test".into(),
                },
            );
        }
    }
    g
}

/// builtin, all Locked, all Provisional, all Open.
fn four_tables() -> Vec<GateTable> {
    vec![
        table(None),
        table(Some(GateState::Locked)),
        table(Some(GateState::Provisional)),
        table(Some(GateState::Open)),
    ]
}

fn mixed_chart() -> Chart {
    let mut c = exercise(Exercise::OnClickQuarters, 60.0, 2);
    let kinds = [
        EventKind::Strum {
            dir: Dir::Down,
            span: None,
            style: Style::Full,
            roll_ms: None,
        },
        EventKind::Mute { dir: Dir::Down },
        EventKind::Ghost { dir: Dir::Up },
        EventKind::Note {
            string: 2,
            fret: 3,
            finger: None,
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
        EventKind::Strum {
            dir: Dir::Down,
            span: None,
            style: Style::Roll,
            roll_ms: None,
        },
        EventKind::Rest,
    ];
    let evs = &mut c.tiers[0].events;
    for (e, k) in evs.iter_mut().zip(kinds) {
        e.kind = k;
        e.chord = Some("C".into());
    }
    c
}

fn charts() -> Vec<(&'static str, Chart)> {
    vec![
        ("demo", demo_chart()),
        ("gallery", gallery_chart()),
        ("quarters", exercise(Exercise::OnClickQuarters, 80.0, 2)),
        ("eighths", exercise(Exercise::OnClickEighths, 80.0, 2)),
        ("mixed", mixed_chart()),
    ]
}

fn requested(ev: &Event) -> Vec<Aspect> {
    use Aspect::*;
    match &ev.kind {
        EventKind::Strum { style, .. } => {
            let mut v = vec![Attack, Timing, Direction, Chord, Strings, Fret];
            match style {
                Style::Accent => v.push(Accent),
                Style::Pm => v.push(Muted),
                _ => {}
            }
            v
        }
        EventKind::Mute { .. } => vec![Attack, Timing, Direction, Muted],
        EventKind::Note { .. } => vec![Attack, Timing, Strings, Fret],
        EventKind::Ghost { .. } => vec![Direction],
        EventKind::Rest => vec![],
    }
}

fn types_of(a: Aspect) -> &'static [VerdictType] {
    match a {
        Aspect::Attack => &[VerdictType::Attack],
        Aspect::Timing => &[VerdictType::Timing],
        Aspect::Direction => &[VerdictType::Direction],
        Aspect::Accent => &[VerdictType::Accent],
        Aspect::Muted => &[VerdictType::Muted],
        Aspect::Chord => &[VerdictType::ChordIdentity],
        Aspect::Strings => &[VerdictType::StringMissing, VerdictType::StringExtra],
        Aspect::Fret => &[VerdictType::WrongFret],
    }
}

// (1) J7: judged is a subset of requested, and of what the gates judge.
#[test]
fn k2_1_judged_is_a_subset_of_requested() {
    for (name, chart) in charts() {
        for gates in four_tables() {
            for cal in [true, false] {
                for (index, ev) in chart.events().iter().enumerate() {
                    let c = contract(&chart, Loc { tier: 0, index }, &gates, cal);
                    let want = requested(ev);
                    for a in Aspect::ALL {
                        assert_eq!(
                            c.requested(a),
                            want.contains(&a),
                            "{name}[{index}] {a:?} requested"
                        );
                        if c.judged(a) {
                            assert!(want.contains(&a), "{name}[{index}] {a:?} judged, not asked");
                            assert!(
                                types_of(a).iter().all(|&t| gates.judges(t)),
                                "{name}[{index}] {a:?} judged behind a locked gate"
                            );
                            if matches!(a, Aspect::Attack | Aspect::Timing) {
                                assert!(cal, "{name}[{index}] {a:?} judged without calibration");
                            }
                        }
                    }
                    if matches!(ev.kind, EventKind::Ghost { .. }) {
                        assert_eq!(c.get(Aspect::Direction), Some(Check::Guide(Why::Ghost)));
                    }
                }
            }
        }
    }
    // Out of range and rests ask for nothing.
    let chart = mixed_chart();
    let none = EventContract::default();
    assert_eq!(
        contract(
            &chart,
            Loc {
                tier: 0,
                index: 999
            },
            &GateTable::builtin(),
            true
        ),
        none
    );
    assert_eq!(
        contract(
            &chart,
            Loc { tier: 9, index: 0 },
            &GateTable::builtin(),
            true
        ),
        none
    );
    assert_eq!(
        contract(
            &chart,
            Loc { tier: 0, index: 7 },
            &GateTable::builtin(),
            true
        ),
        none
    );
}

// (2) builtin: only Attack and Timing are judged; no calibration makes them guide.
#[test]
fn k2_2_builtin_judges_only_attack_and_timing() {
    let chart = mixed_chart();
    let gates = GateTable::builtin();
    for index in 0..chart.events().len() {
        let c = contract(&chart, Loc { tier: 0, index }, &gates, true);
        for a in Aspect::ALL {
            let expect = matches!(a, Aspect::Attack | Aspect::Timing) && c.requested(a);
            assert_eq!(c.judged(a), expect, "[{index}] {a:?}");
        }
        let c = contract(&chart, Loc { tier: 0, index }, &gates, false);
        if c.requested(Aspect::Timing) {
            assert_eq!(
                c.get(Aspect::Timing),
                Some(Check::Guide(Why::NoCalibration))
            );
            assert_eq!(
                c.get(Aspect::Attack),
                Some(Check::Guide(Why::NoCalibration))
            );
        }
        for a in Aspect::ALL {
            assert!(!c.judged(a), "[{index}] {a:?} judged with no calibration");
        }
    }
    // A locked gate says so; a requested, locked aspect is never silently dropped.
    let c = contract(&chart, Loc { tier: 0, index: 0 }, &gates, true);
    assert_eq!(c.get(Aspect::Chord), Some(Check::Guide(Why::GateLocked)));
    assert_eq!(c.get(Aspect::Strings), Some(Check::Guide(Why::GateLocked)));
    assert!(matches!(
        c.get(Aspect::Attack),
        Some(Check::Judged(GateState::Provisional))
    ));
}

// (3) a ghost is all guide.
#[test]
fn k2_3_ghost_is_all_guide() {
    let chart = mixed_chart();
    for gates in four_tables() {
        for cal in [true, false] {
            let c = contract(&chart, Loc { tier: 0, index: 2 }, &gates, cal);
            for a in Aspect::ALL {
                assert!(!c.judged(a));
                if a != Aspect::Direction {
                    assert!(!c.requested(a));
                }
            }
            assert_eq!(c.get(Aspect::Direction), Some(Check::Guide(Why::Ghost)));
        }
    }
}

// (4) agreement with the pulse judge.
const FS: f64 = 48_000.0;
const BASE: f64 = 1_000_000.0;

/// Play every event of `chart` through the judge (skipping every fourth one) and return
/// the final verdicts by event.
fn run_round(chart: &Chart, gates: &GateTable, timing: TimingRef) -> Vec<(usize, VerdictLog)> {
    let cfg = JudgeConfig::new(FS, Level::Apprentice);
    let mut j = PulseJudge::new(cfg, timing, gates);
    let off = timing.offset_ms().unwrap_or(0.0);
    let frame = |ms: f64| BASE + ms * FS / 1000.0;
    // Event index and its time in ms; rests are not expected.
    let evs: Vec<(usize, f64, bool)> = chart
        .events()
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e.kind, EventKind::Rest))
        .map(|(i, e)| {
            (
                i,
                chart.seconds_of_tick(e.tick) * 1000.0,
                matches!(e.kind, EventKind::Ghost { .. }),
            )
        })
        .collect();
    let gap = 1000.0;
    let mut items: Vec<(f64, u8, JudgeInput)> = Vec::new();
    for (id, &(i, t, ghost)) in evs.iter().enumerate() {
        items.push((
            t - 2000.0,
            0,
            JudgeInput::Expect {
                id: id as u32,
                g_in: frame(t),
                kind: if ghost {
                    ExpectKind::Ghost
                } else {
                    ExpectKind::Attack
                },
                prev_gap_s: gap / 1000.0,
                next_gap_s: gap / 1000.0,
                pos_in_bar: (id % 8) as u8,
            },
        ));
        if !ghost && i % 4 != 3 {
            let t_in = frame(t + off + 12.0);
            items.push((t + off + 17.0, 1, JudgeInput::Heard { t_in, judged: true }));
            items.push((t + off + 72.0, 2, JudgeInput::Strum { t_in, judged: true }));
        }
    }
    let end = evs.last().map_or(0.0, |e| e.1) + 3000.0;
    let mut now = -2500.0;
    while now <= end {
        items.push((
            now,
            4,
            JudgeInput::Progress {
                in_now: frame(now),
                locked: true,
            },
        ));
        now += 10.0;
    }
    items.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut log = VerdictLog::new();
    let mut out: Vec<JudgeOut> = Vec::new();
    for (_, _, i) in items {
        out.clear();
        j.push(&i, &mut out);
        for o in &out {
            log.push(o);
        }
    }
    evs.iter()
        .enumerate()
        .map(|(id, &(i, _, _))| {
            let mut l = VerdictLog::new();
            for v in log.verdicts().iter().filter(|v| v.id == id as u32) {
                l.push(&JudgeOut::Verdict(*v));
            }
            (i, l)
        })
        .collect()
}

#[test]
fn k2_4_agrees_with_the_pulse_judge() {
    let calibrated = TimingRef::loopback(87.3, 0.5);
    let setups: Vec<(GateTable, TimingRef)> = vec![
        (table(None), calibrated),
        (table(None), TimingRef::none()),
        (table(Some(GateState::Locked)), calibrated),
        (table(Some(GateState::Provisional)), calibrated),
        (table(Some(GateState::Open)), calibrated),
    ];
    let mut saw_judged_attack = false;
    let mut saw_not_judged_attack = false;
    for (name, chart) in [
        ("mixed", mixed_chart()),
        ("quarters", exercise(Exercise::OnClickQuarters, 60.0, 2)),
    ] {
        for (n, (gates, timing)) in setups.iter().enumerate() {
            let cal = timing.judges_timing();
            let round = run_round(&chart, gates, *timing);
            for (index, log) in round {
                let c = contract(&chart, Loc { tier: 0, index }, gates, cal);
                let vs = log.verdicts();
                assert_eq!(
                    vs.len(),
                    1,
                    "{name} setup {n} event {index}: one final verdict"
                );
                let v = vs[0];
                let ctx = format!("{name} setup {n} event {index}");
                let a_judged = !matches!(v.attack, Attack::NotJudged(_));
                assert_eq!(a_judged, c.judged(Aspect::Attack), "{ctx} attack");
                saw_judged_attack |= a_judged;
                saw_not_judged_attack |= !a_judged;
                // No attack heard: there is nothing to time, by design.
                if matches!(v.attack, Attack::NotHeard) {
                    assert!(
                        matches!(v.timing, Timing::NotJudged(Why::NoAttack)),
                        "{ctx}"
                    );
                } else {
                    let t_judged = !matches!(v.timing, Timing::NotJudged(_));
                    assert_eq!(t_judged, c.judged(Aspect::Timing), "{ctx} timing");
                }
                let d_judged = !matches!(v.dir, guitarness_core::verdict::Direction::NotJudged(_));
                assert_eq!(d_judged, c.judged(Aspect::Direction), "{ctx} direction");
            }
        }
    }
    assert!(
        saw_judged_attack && saw_not_judged_attack,
        "the round must reach both sides"
    );
}
