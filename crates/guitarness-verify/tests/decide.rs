//! Decision rule against hand-built witness outputs (no DSP involved).

use guitarness_verify::decide::decide;
use guitarness_verify::types::*;
use tonica_core::gates::{ALL_TYPES, GateEntry, GateEvidence, GateRule, GateState, GateTable};
use tonica_core::verdict::VerdictType;

fn hyp(kind: HypKind) -> Hypothesis {
    Hypothesis {
        kind,
        sounding: [-1; 6],
    }
}

/// alts: bit0 = Missing(2), bit1 = Extra(0), bit2 = WrongFret(1,3), bit3 = Neighbour, bit4 = AllMuted.
fn set() -> ConfusionSet {
    ConfusionSet {
        chord: "C".into(),
        ask: [Ask::May; 6],
        expected: hyp(HypKind::Expected),
        alts: vec![
            hyp(HypKind::Missing { string: 2 }),
            hyp(HypKind::Extra { string: 0 }),
            hyp(HypKind::WrongFret { string: 1, fret: 3 }),
            hyp(HypKind::Neighbour { chord: "G".into() }),
            hyp(HypKind::AllMuted),
        ],
        tuning: [0; 6],
        capo: 0,
    }
}

fn cfg(accuse: Option<f32>) -> VerifyConfig {
    VerifyConfig {
        ok_margin: 2.0,
        veto_margin: 3.0,
        accuse_margin: accuse,
    }
}

fn entry(state: GateState) -> GateEntry {
    GateEntry {
        state,
        rule: GateRule::default(),
        evidence: GateEvidence::default(),
        reason: "test".into(),
    }
}

/// Every verdict type set to `state`, so chord and accused-type gates move together.
fn gates_all(state: GateState) -> GateTable {
    let mut g = GateTable::builtin();
    for t in ALL_TYPES {
        g.set(t, entry(state));
    }
    g
}

fn gates_with(chord: GateState, accused: VerdictType, st: GateState) -> GateTable {
    let mut g = GateTable::builtin();
    g.set(VerdictType::ChordIdentity, entry(chord));
    g.set(accused, entry(st));
    g
}

fn names(alts: u32, margin: f32) -> Opinion {
    Opinion::Names { alts, margin }
}

fn sup(margin: f32) -> Opinion {
    Opinion::Supports { margin }
}

const D: Family = Family::Dsp;
const L: Family = Family::Learned;

fn why(v: &ChordVerdict) -> Abstain {
    match v.outcome {
        ChordOutcome::NotJudged(a) => a,
        ref o => panic!("expected NotJudged, got {o:?}"),
    }
}

#[test]
fn builtin_gates_always_locked() {
    let g = GateTable::builtin();
    let ops_list: Vec<Vec<(Family, Opinion)>> = vec![
        vec![],
        vec![(D, sup(9.0))],
        vec![(D, names(1, 9.0)), (L, names(1, 9.0))],
        vec![(D, Opinion::Abstains(Abstain::LowSnr))],
    ];
    for ops in ops_list {
        for a in [None, Some(0.0), Some(5.0)] {
            assert_eq!(why(&decide(&set(), &ops, &g, &cfg(a))), Abstain::GateLocked);
        }
    }
}

#[test]
fn ok_rules() {
    let g = gates_all(GateState::Provisional);
    let c = cfg(None);
    let v = decide(&set(), &[(D, sup(2.5))], &g, &c);
    assert_eq!(v.outcome, ChordOutcome::Ok);
    assert_eq!(v.vtype, VerdictType::ChordIdentity);
    // Below ok_margin
    assert_eq!(
        why(&decide(&set(), &[(D, sup(1.0))], &g, &c)),
        Abstain::NoMargin
    );
    // Weak Names (below veto) does not block Ok
    assert_eq!(
        decide(&set(), &[(D, sup(2.5)), (L, names(1, 1.0))], &g, &c).outcome,
        ChordOutcome::Ok
    );
    // Strong Names blocks Ok: conflict
    assert_eq!(
        why(&decide(
            &set(),
            &[(D, sup(9.0)), (L, names(1, 3.0))],
            &g,
            &c
        )),
        Abstain::Conflict
    );
    // Supports plus an abstention is still Ok
    assert_eq!(
        decide(
            &set(),
            &[(D, sup(2.5)), (L, Opinion::Abstains(Abstain::Unavailable))],
            &g,
            &c
        )
        .outcome,
        ChordOutcome::Ok
    );
}

#[test]
fn abstention_reasons() {
    let g = gates_all(GateState::Open);
    let c = cfg(Some(1.0));
    assert_eq!(why(&decide(&set(), &[], &g, &c)), Abstain::Unavailable);
    assert_eq!(
        why(&decide(
            &set(),
            &[(D, Opinion::Abstains(Abstain::Unobservable))],
            &g,
            &c
        )),
        Abstain::Unobservable
    );
    assert_eq!(
        why(&decide(&set(), &[(D, sup(f32::NAN))], &g, &c)),
        Abstain::Unavailable
    );
    assert_eq!(
        why(&decide(&set(), &[(D, names(1, 99.0))], &g, &c)),
        Abstain::OneWitness
    );
}

#[derive(Clone, Copy, Debug)]
enum Gate {
    Locked,
    Provisional,
    Open,
}

/// Full enumeration; `Wrong` must appear in exactly one equivalence class of inputs.
#[test]
fn enumeration_wrong_only_in_allowed_cell_and_never_when_locked() {
    let state = |g: Gate| match g {
        Gate::Locked => GateState::Locked,
        Gate::Provisional => GateState::Provisional,
        Gate::Open => GateState::Open,
    };
    // Per-witness opinion menu: Supports, Names same (bit 0), Names different (bit 1), Abstains.
    let menu = |m: f32| {
        [
            sup(m),
            names(0b1, m),
            names(0b10, m),
            Opinion::Abstains(Abstain::LowSnr),
        ]
    };
    let accuses = [None, Some(5.0), Some(2.0)]; // None, above margin 4.0, below margin 4.0
    let mut wrong_cells = 0;
    let mut total = 0;
    for chord in [Gate::Locked, Gate::Provisional, Gate::Open] {
        for acc_g in [Gate::Locked, Gate::Provisional, Gate::Open] {
            let gates = gates_with(state(chord), VerdictType::StringMissing, state(acc_g));
            for n in 0..=2usize {
                for fams in [(D, D), (D, L)] {
                    for a in menu(4.0) {
                        for b in menu(4.0) {
                            for acc in accuses {
                                let all = [(fams.0, a), (fams.1, b)];
                                let ops = &all[..n];
                                let v = decide(&set(), ops, &gates, &cfg(acc));
                                total += 1;
                                if let ChordOutcome::Wrong(k) = &v.outcome {
                                    wrong_cells += 1;
                                    // The single allowed combination.
                                    assert!(
                                        matches!(chord, Gate::Open),
                                        "Wrong needs an Open chord gate"
                                    );
                                    assert!(
                                        matches!(acc_g, Gate::Open),
                                        "Wrong needs an Open accused gate"
                                    );
                                    assert_eq!(n, 2);
                                    assert_ne!(fams.0, fams.1.clone(), "same family");
                                    assert!(matches!(a, Opinion::Names { alts: 1, .. }));
                                    assert!(matches!(b, Opinion::Names { alts: 1, .. }));
                                    assert_eq!(
                                        acc,
                                        Some(2.0),
                                        "margin 4.0 only clears the lower threshold"
                                    );
                                    assert_eq!(*k, HypKind::Missing { string: 2 });
                                    assert_eq!(v.vtype, VerdictType::StringMissing);
                                }
                                if matches!(chord, Gate::Locked) {
                                    assert_eq!(
                                        v.outcome,
                                        ChordOutcome::NotJudged(Abstain::GateLocked)
                                    );
                                }
                                if matches!(acc_g, Gate::Provisional) {
                                    assert!(!matches!(v.outcome, ChordOutcome::Wrong(_)));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // Exactly one cell of the grid above: chord in {Provisional, Open} (2) x one input class.
    assert_eq!(wrong_cells, 1);
    assert_eq!(total, 3 * 3 * 3 * 2 * 16 * 3, "grid size");
}

#[test]
fn accusation_details() {
    let g = gates_all(GateState::Open);
    let c = cfg(Some(2.0));
    // Same family twice: one witness only.
    assert_eq!(
        why(&decide(
            &set(),
            &[(D, names(1, 9.0)), (D, names(1, 9.0))],
            &g,
            &c
        )),
        Abstain::OneWitness
    );
    // One Names, one abstain.
    assert_eq!(
        why(&decide(
            &set(),
            &[(D, names(1, 9.0)), (L, Opinion::Abstains(Abstain::LowSnr))],
            &g,
            &c
        )),
        Abstain::OneWitness
    );
    // Disjoint masks: conflict; overlapping masks keeping two bits: ambiguous.
    assert_eq!(
        why(&decide(
            &set(),
            &[(D, names(1, 9.0)), (L, names(2, 9.0))],
            &g,
            &c
        )),
        Abstain::Conflict
    );
    assert_eq!(
        why(&decide(
            &set(),
            &[(D, names(0b11, 9.0)), (L, names(0b11, 9.0))],
            &g,
            &c
        )),
        Abstain::Ambiguous
    );
    // Intersection narrows to one hypothesis.
    let v = decide(
        &set(),
        &[(D, names(0b101, 9.0)), (L, names(0b100, 9.0))],
        &g,
        &c,
    );
    assert_eq!(
        v.outcome,
        ChordOutcome::Wrong(HypKind::WrongFret { string: 1, fret: 3 })
    );
    assert_eq!(v.vtype, VerdictType::WrongFret);
    // Margin is the minimum of the two; below accuse_margin abstains.
    assert_eq!(
        why(&decide(
            &set(),
            &[(D, names(1, 9.0)), (L, names(1, 1.9))],
            &g,
            &c
        )),
        Abstain::NoMargin
    );
    // Mask bit past the end of alts: malformed, abstain.
    assert_eq!(
        why(&decide(
            &set(),
            &[(D, names(1 << 20, 9.0)), (L, names(1 << 20, 9.0))],
            &g,
            &c
        )),
        Abstain::Conflict
    );
    // A Supports next to a vetoing pair blocks the accusation.
    assert_eq!(
        why(&decide(
            &set(),
            &[(D, names(1, 9.0)), (L, names(1, 9.0)), (D, sup(9.0))],
            &g,
            &c
        )),
        Abstain::Conflict
    );
    // AllMuted maps to StringMissing, Neighbour to ChordIdentity (vtype follows vtype_of).
    let v = decide(
        &set(),
        &[(D, names(1 << 4, 9.0)), (L, names(1 << 4, 9.0))],
        &g,
        &c,
    );
    assert_eq!(
        (v.outcome, v.vtype),
        (
            ChordOutcome::Wrong(HypKind::AllMuted),
            VerdictType::StringMissing
        )
    );
    let v = decide(
        &set(),
        &[(D, names(1 << 3, 9.0)), (L, names(1 << 3, 9.0))],
        &g,
        &c,
    );
    assert_eq!(v.vtype, VerdictType::ChordIdentity);
}

/// Whatever the opinions and thresholds, `Wrong` needs the accused type's gate `Open`
/// and the chord gate not `Locked`; checked for every accusable hypothesis.
#[test]
fn no_accusation_unless_accused_gate_open() {
    let s = set();
    for bit in 0..s.alts.len() as u32 {
        let t = vtype_of(&s.alts[bit as usize].kind);
        for chord in [GateState::Locked, GateState::Provisional, GateState::Open] {
            for st in [GateState::Locked, GateState::Provisional, GateState::Open] {
                let g = gates_with(chord, t, st);
                let ops = [(D, names(1 << bit, 99.0)), (L, names(1 << bit, 99.0))];
                let v = decide(&s, &ops, &g, &cfg(Some(0.0)));
                // Neighbour is judged by the chord gate itself, so `st` overrides `chord`.
                let chord_now = if t == VerdictType::ChordIdentity {
                    st
                } else {
                    chord
                };
                let expect_wrong = chord_now == GateState::Open && st == GateState::Open;
                assert_eq!(
                    matches!(v.outcome, ChordOutcome::Wrong(_)),
                    expect_wrong,
                    "bit {bit} {chord:?} {st:?}"
                );
            }
        }
    }
}

#[test]
fn bad_config_and_expected_alt_abstain() {
    let g = gates_all(GateState::Open);
    let strong = [(D, names(1, 9.0)), (L, names(1, 9.0)), (D, sup(9.0))];
    let one = [(D, sup(9.0))];
    for veto in [f32::NAN, f32::INFINITY] {
        let c = VerifyConfig {
            ok_margin: 2.0,
            veto_margin: veto,
            accuse_margin: Some(1.0),
        };
        assert_eq!(why(&decide(&set(), &strong, &g, &c)), Abstain::Unavailable);
    }
    let c = VerifyConfig {
        ok_margin: f32::NAN,
        veto_margin: 3.0,
        accuse_margin: None,
    };
    assert_eq!(why(&decide(&set(), &one, &g, &c)), Abstain::Unavailable);
    let c = VerifyConfig {
        ok_margin: 2.0,
        veto_margin: 3.0,
        accuse_margin: Some(f32::NEG_INFINITY),
    };
    let two = [(D, names(1, 9.0)), (L, names(1, 9.0))];
    assert_eq!(why(&decide(&set(), &two, &g, &c)), Abstain::Unavailable);
    // Locked still wins over a bad config.
    let locked = GateTable::builtin();
    assert_eq!(why(&decide(&set(), &two, &locked, &c)), Abstain::GateLocked);
    // An Expected entry inside alts is never accused.
    let mut s = set();
    s.alts[0] = hyp(HypKind::Expected);
    assert_eq!(
        why(&decide(&s, &two, &g, &cfg(Some(1.0)))),
        Abstain::Conflict
    );
    // Chord gate Provisional blocks accusation even with the accused gate Open.
    let g2 = gates_with(
        GateState::Provisional,
        VerdictType::StringMissing,
        GateState::Open,
    );
    assert_eq!(
        why(&decide(&set(), &two, &g2, &cfg(Some(1.0)))),
        Abstain::GateLocked
    );
}
