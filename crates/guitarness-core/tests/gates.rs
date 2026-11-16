use guitarness_core::gates::*;
use guitarness_core::report::*;
use guitarness_core::verdict::VerdictType as V;

fn obs(o: Outcome, truth: Truth) -> Observation {
    Observation {
        vtype: V::Timing,
        outcome: o,
        truth,
        borderline: false,
        source: TapeSource::Synthetic,
        tape: 0,
    }
}

fn report(list: &[Observation]) -> TypeReport {
    ConfusionReport::build(list, &GateTable::builtin())
        .get(V::Timing)
        .clone()
}

fn many(
    n: u32,
    o: Outcome,
    truth: Truth,
    src: TapeSource,
    tape0: u32,
    tapes: u32,
) -> Vec<Observation> {
    (0..n)
        .map(|i| Observation {
            source: src,
            tape: tape0 + i % tapes,
            ..obs(o, truth)
        })
        .collect()
}

#[test]
fn builtin_defaults() {
    let g = GateTable::builtin();
    for t in [V::Attack, V::Timing, V::Tendency] {
        assert_eq!(g.state(t), GateState::Provisional, "{t:?}");
        assert!(g.judges(t));
    }
    for t in ALL_TYPES {
        if !matches!(t, V::Attack | V::Timing | V::Tendency) {
            assert_eq!(g.state(t), GateState::Locked, "{t:?}");
            assert!(!g.judges(t));
        }
    }
    assert!(g.judges(V::Timing));
}

#[test]
fn counts_case_1_all_right() {
    let r = report(&[obs(Outcome::Ok, Truth::Right); 3]);
    assert_eq!(
        (r.ok, r.n, r.false_wrong, r.false_ok, r.abstain),
        (3, 3, 0, 0, 0)
    );
}

#[test]
fn counts_case_2_false_accusation() {
    let r = report(&[
        obs(Outcome::Wrong, Truth::Right),
        obs(Outcome::Ok, Truth::Right),
    ]);
    assert_eq!((r.wrong, r.n, r.false_wrong, r.false_ok), (1, 2, 1, 0));
    assert!((r.false_wrong_rate - 0.5).abs() < 1e-12);
}

#[test]
fn counts_case_3_true_accusation_and_not_heard() {
    let r = report(&[
        obs(Outcome::Wrong, Truth::Wrong),
        obs(Outcome::NotHeard, Truth::Wrong),
    ]);
    assert_eq!((r.wrong, r.not_heard, r.n, r.false_wrong), (1, 1, 2, 0));
}

#[test]
fn counts_case_4_not_heard_on_played_is_false_accusation() {
    let r = report(&[obs(Outcome::NotHeard, Truth::Right)]);
    assert_eq!((r.false_wrong, r.false_ok, r.n), (1, 0, 1));
}

#[test]
fn counts_case_5_false_accept_and_abstain_are_separate() {
    let r = report(&[
        obs(Outcome::Ok, Truth::Wrong),
        obs(Outcome::NotJudged, Truth::Wrong),
        obs(Outcome::NotJudged, Truth::Right),
        obs(Outcome::Ok, Truth::Right),
    ]);
    assert_eq!(
        (r.false_ok, r.abstain, r.false_wrong, r.not_judged, r.n),
        (1, 2, 0, 2, 2)
    );
    assert!((r.abstain_rate - 0.5).abs() < 1e-12);
}

#[test]
fn counts_case_6_unlabeled_disputed_borderline_stay_out_of_rates() {
    let mut b = obs(Outcome::Wrong, Truth::Right);
    b.borderline = true;
    let r = report(&[
        obs(Outcome::Wrong, Truth::Unlabeled),
        obs(Outcome::Disputed, Truth::Right),
        b,
        obs(Outcome::Ok, Truth::Right),
    ]);
    assert_eq!(
        (r.wrong, r.disputed, r.borderline, r.n, r.false_wrong),
        (2, 1, 1, 1, 0)
    );
}

#[test]
fn wilson_reference_values() {
    // Hand-computed (Wilson score, z = 1.959964).
    let near = |a: f64, b: f64| assert!((a - b).abs() < 5e-4, "{a} vs {b}");
    near(wilson95(0, 10).1, 0.2775);
    let (lo, hi) = wilson95(1, 10);
    near(lo, 0.0179);
    near(hi, 0.4045);
    let (lo, hi) = wilson95(5, 10);
    near(lo, 0.2366);
    near(hi, 0.7634);
    near(wilson95(10, 10).0, 0.7225);
    // n = 200, 0 false: 3.8415/200 / (1 + 3.8415/200) = 0.0188 (the plan's 1.5% is the rule of three).
    near(wilson95(0, 200).1, 0.01884);
    assert_eq!(wilson95(0, 0), (0.0, 1.0));
}

#[test]
fn rule_boundaries() {
    let rule = GateRule::default();
    assert_eq!(rule.check(199, 0), GateCheck::TooFew);
    assert_eq!(rule.check(200, 0), GateCheck::Pass);
    assert_eq!(rule.check(200, 1), GateCheck::Pass);
    assert_eq!(rule.check(200, 2), GateCheck::Fail);
    assert_eq!(rule.check(400, 2), GateCheck::Pass);
    assert_eq!(rule.check(400, 3), GateCheck::Fail);
}

#[test]
fn report_applies_rule_to_counts() {
    let mut v = many(199, Outcome::Ok, Truth::Right, TapeSource::Synthetic, 0, 2);
    assert_eq!(report(&v).gate, GateCheck::TooFew);
    v.push(obs(Outcome::Wrong, Truth::Right));
    let r = report(&v);
    assert_eq!((r.n, r.false_wrong, r.gate), (200, 1, GateCheck::Pass));
    let mut v = many(397, Outcome::Ok, Truth::Right, TapeSource::Synthetic, 0, 2);
    v.extend(many(
        3,
        Outcome::Wrong,
        Truth::Right,
        TapeSource::Synthetic,
        0,
        2,
    ));
    let r = report(&v);
    assert_eq!((r.n, r.false_wrong, r.gate), (400, 3, GateCheck::Fail));
}

#[test]
fn synthetic_evidence_never_opens() {
    let v = many(
        1000,
        Outcome::Ok,
        Truth::Right,
        TapeSource::Synthetic,
        0,
        50,
    );
    let r = report(&v);
    assert_eq!(
        (r.gate, r.earned, r.synthetic_tapes, r.real_tapes),
        (GateCheck::Pass, GateState::Provisional, 50, 0)
    );
    let mut t = GateTable::builtin();
    let p = ConfusionReport::build(&v, &t).proposed_gates(&t);
    assert_eq!(p.state(V::Timing), GateState::Provisional);
    t.apply_evidence(
        V::Attack,
        GateEvidence {
            events: 5000,
            false_wrong: 0,
            synthetic_tapes: 99,
            real_tapes: 0,
        },
        "x",
    );
    assert_eq!(t.state(V::Attack), GateState::Provisional);
}

#[test]
fn real_tapes_open_only_with_enough_tapes() {
    let mut v = many(300, Outcome::Ok, Truth::Right, TapeSource::Real, 10, 2);
    assert_eq!(report(&v).earned, GateState::Provisional);
    v.extend(many(30, Outcome::Ok, Truth::Right, TapeSource::Real, 20, 1));
    let r = report(&v);
    assert_eq!((r.real_tapes, r.earned), (3, GateState::Open));
    // A failing rule locks even with real tapes.
    v.extend(many(
        5,
        Outcome::Wrong,
        Truth::Right,
        TapeSource::Real,
        10,
        1,
    ));
    assert_eq!(report(&v).earned, GateState::Locked);
}

#[test]
fn json_round_trip_and_unknown_type() {
    let mut t = GateTable::builtin();
    t.apply_evidence(
        V::Muted,
        GateEvidence {
            events: 250,
            false_wrong: 1,
            synthetic_tapes: 1,
            real_tapes: 4,
        },
        "tapes",
    );
    assert_eq!(t.state(V::Muted), GateState::Open);
    let back = GateTable::from_json(&t.to_json()).unwrap();
    assert_eq!(back, t);
    assert_eq!(
        GateTable::from_json(&GateTable::builtin().to_json()).unwrap(),
        GateTable::builtin()
    );

    let mut v: serde_json::Value = serde_json::from_str(&t.to_json()).unwrap();
    let g = v["gates"].as_object_mut().unwrap();
    let entry = g["timing"].clone();
    g.insert("vibrato".into(), entry);
    let loaded = GateTable::from_json(&v.to_string()).unwrap();
    assert_eq!(loaded, t, "unknown type ignored, rest unchanged");
    assert!(GateTable::from_json("{").is_err());
    assert!(GateTable::from_json(r#"{"version":2,"gates":{}}"#).is_err());
}

#[test]
fn unbacked_open_claim_is_demoted_to_locked() {
    let mut v: serde_json::Value = serde_json::from_str(&GateTable::builtin().to_json()).unwrap();
    v["gates"]["accent"]["state"] = "open".into();
    let t = GateTable::from_json(&v.to_string()).unwrap();
    assert_eq!(t.state(V::Accent), GateState::Locked);
    assert!(!t.judges(V::Accent));
}

#[test]
fn report_outputs_render() {
    let r = ConfusionReport::build(
        &many(4, Outcome::Ok, Truth::Right, TapeSource::Synthetic, 0, 1),
        &GateTable::builtin(),
    );
    let text = r.to_text();
    assert!(text.contains("timing") && text.contains("chord_identity") && text.contains("Wilson"));
    let j: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    assert_eq!(j["types"].as_array().unwrap().len(), 10);
}

// ---- K1: synthetic-evidence lock ----

const LOCKED_WITHOUT_REAL: [V; 7] = [
    V::Direction,
    V::Accent,
    V::Muted,
    V::ChordIdentity,
    V::StringMissing,
    V::StringExtra,
    V::WrongFret,
];

fn perfect(vtype: V, src: TapeSource, tape0: u32, tapes: u32) -> Vec<Observation> {
    many(1000, Outcome::Ok, Truth::Right, src, tape0, tapes)
        .into_iter()
        .map(|o| Observation { vtype, ..o })
        .collect()
}

fn perfect_evidence(real_tapes: u32) -> GateEvidence {
    GateEvidence {
        events: 5000,
        false_wrong: 0,
        synthetic_tapes: 99,
        real_tapes,
    }
}

#[test]
fn synthetic_can_unlock_table() {
    for t in [V::Attack, V::Timing, V::Tendency] {
        assert!(synthetic_can_unlock(t), "{t:?}");
    }
    for t in LOCKED_WITHOUT_REAL {
        assert!(!synthetic_can_unlock(t), "{t:?}");
    }
}

#[test]
fn perfect_synthetic_evidence_does_not_grant_provisional_to_chord_types() {
    for t in LOCKED_WITHOUT_REAL {
        let mut g = GateTable::builtin();
        g.apply_evidence(t, perfect_evidence(0), "synthetic");
        assert_eq!(g.state(t), GateState::Locked, "direct {t:?}");
        assert!(!g.judges(t));
        assert!(g.entry(t).reason.contains("synthetic"), "reason written");

        let list = perfect(t, TapeSource::Synthetic, 0, 50);
        let base = GateTable::builtin();
        let p = ConfusionReport::build(&list, &base).proposed_gates(&base);
        assert_eq!(p.state(t), GateState::Locked, "proposed {t:?}");
    }
}

#[test]
fn three_real_tapes_open_chord_identity() {
    let mut g = GateTable::builtin();
    g.apply_evidence(V::ChordIdentity, perfect_evidence(3), "real");
    assert_eq!(g.state(V::ChordIdentity), GateState::Open);

    let mut list = perfect(V::ChordIdentity, TapeSource::Real, 10, 3);
    list.extend(perfect(V::ChordIdentity, TapeSource::Synthetic, 0, 5));
    let base = GateTable::builtin();
    let p = ConfusionReport::build(&list, &base).proposed_gates(&base);
    assert_eq!(p.state(V::ChordIdentity), GateState::Open);

    // One real tape is real evidence but not enough to open: Provisional stays.
    let mut g = GateTable::builtin();
    g.apply_evidence(V::ChordIdentity, perfect_evidence(1), "one real tape");
    assert_eq!(g.state(V::ChordIdentity), GateState::Provisional);
}

#[test]
fn attack_and_timing_still_earn_provisional_from_synthetic() {
    for t in [V::Attack, V::Timing, V::Tendency] {
        let mut g = GateTable::builtin();
        g.apply_evidence(t, perfect_evidence(0), "synthetic");
        assert_eq!(g.state(t), GateState::Provisional, "{t:?}");
    }
}

#[test]
fn set_stays_free_and_json_demotes_provisional_without_real_tapes() {
    // GateTable::set is the bench's escape hatch.
    let mut t = GateTable::builtin();
    let mut e = t.entry(V::ChordIdentity).clone();
    e.state = GateState::Provisional;
    t.set(V::ChordIdentity, e);
    assert_eq!(t.state(V::ChordIdentity), GateState::Provisional);

    // A file claiming Provisional for a chord type without real tapes loads Locked.
    let loaded = GateTable::from_json(&t.to_json()).unwrap();
    assert_eq!(loaded.state(V::ChordIdentity), GateState::Locked);
    assert!(loaded.entry(V::ChordIdentity).reason.contains("synthetic"));

    // Round trip of an honest table (Open with real tapes, builtin Provisional) is unchanged.
    let mut ok = GateTable::builtin();
    ok.apply_evidence(V::ChordIdentity, perfect_evidence(4), "tapes");
    assert_eq!(GateTable::from_json(&ok.to_json()).unwrap(), ok);
    let b = GateTable::builtin();
    assert_eq!(GateTable::from_json(&b.to_json()).unwrap(), b);
}
