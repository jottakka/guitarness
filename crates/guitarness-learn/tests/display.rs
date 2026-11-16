use guitarness_core::gates::GateState;
use guitarness_core::pulse::Exercise;
use guitarness_core::verdict::VerdictType;
use guitarness_learn::display::{DisplayLedger, Mono};
use guitarness_learn::types::*;

/// SplitMix64.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn block(
    ts_ms: i64,
    kind: BlockKind,
    bpm: u16,
    ex: Exercise,
    strums: u32,
    dur_ms: u32,
) -> BlockRecord {
    BlockRecord {
        ts_ms,
        dur_ms,
        session: 1,
        kind,
        spec: BlockSpec {
            exercise: ex,
            bpm,
            bars: 4,
        },
        after_gap: false,
        strums,
        evidence: vec![Evidence {
            id: 0,
            skill: SkillKey("pulse:q".into()),
            vtype: VerdictType::Attack,
            outcome: Outcome::Zero(ZeroWhy::NotHeard),
            gate: GateState::Open,
            dt_ms: None,
        }],
        prov: Provenance {
            judge_v: "t".into(),
            basis: "t".into(),
            gate: GateState::Open,
            tape: None,
        },
    }
}

fn grades(g: Grade) -> Vec<(SkillKey, Grade)> {
    vec![(SkillKey("pulse:q".into()), g)]
}

fn snapshot(l: &DisplayLedger) -> Vec<u64> {
    let mut v = vec![
        l.strums.get(),
        l.blocks.get(),
        l.active_ms.get(),
        l.days.get(),
        l.best_block_strums.get(),
        l.firsts.len() as u64,
    ];
    for k in ["quarters", "eighths"] {
        v.push(l.best_bpm.get(k).map_or(0, |m| m.get()));
    }
    v
}

fn assert_not_lower(before: &[u64], after: &[u64], ctx: &str) {
    for (i, (b, a)) in before.iter().zip(after).enumerate() {
        assert!(a >= b, "{ctx}: field {i} fell {b} -> {a}");
    }
}

fn random_block(r: &mut Rng) -> (BlockRecord, Vec<(SkillKey, Grade)>) {
    let kind = match r.below(4) {
        0 => BlockKind::Probe,
        1 => BlockKind::Familiar,
        2 => BlockKind::Recover,
        _ => BlockKind::Stretch,
    };
    let ex = if r.below(2) == 0 {
        Exercise::OnClickQuarters
    } else {
        Exercise::OnClickEighths
    };
    let strums = if r.below(5) == 0 {
        0
    } else {
        r.below(200) as u32
    };
    let b = block(
        r.below(40 * 86_400_000) as i64 - 5 * 86_400_000,
        kind,
        20 + r.below(220) as u16,
        ex,
        strums,
        r.below(120_000) as u32,
    );
    let g = match r.below(5) {
        0 => vec![],
        1 => grades(Grade::Good),
        2 => grades(Grade::Fair),
        3 => grades(Grade::Weak),
        _ => grades(Grade::NoReading),
    };
    (b, g)
}

// (1) J2
#[test]
fn j2_no_number_ever_falls() {
    let mut r = Rng(42);
    for seq in 0..10_000 {
        let mut l = DisplayLedger::new();
        let mut prev = snapshot(&l);
        for _ in 0..(1 + r.below(8)) {
            match r.below(6) {
                0 => {
                    // dispute
                    l.on_dispute(r.below(1000) as i64, r.below(10) as u32);
                }
                1 => {
                    // reload: possibly stale or tampered file
                    let mut stale = DisplayLedger::new();
                    for _ in 0..r.below(3) {
                        let (b, g) = random_block(&mut r);
                        stale.apply(&b, &g);
                    }
                    let json = stale.to_json();
                    let loaded = DisplayLedger::from_json(&json).unwrap();
                    l.merge(&loaded);
                }
                _ => {
                    let (b, g) = random_block(&mut r);
                    l.apply(&b, &g);
                }
            }
            let now = snapshot(&l);
            assert_not_lower(&prev, &now, &format!("seq {seq}"));
            prev = now;
        }
        let rt = DisplayLedger::from_json(&l.to_json()).unwrap();
        assert_eq!(rt, l, "round trip, seq {seq}");
    }
}

// (2) merge with older / tampered-low file
#[test]
fn merge_with_old_or_tampered_file_lowers_nothing() {
    let mut l = DisplayLedger::new();
    l.apply(
        &block(
            86_400_000,
            BlockKind::Stretch,
            100,
            Exercise::OnClickQuarters,
            50,
            30_000,
        ),
        &grades(Grade::Good),
    );
    l.apply(
        &block(
            3 * 86_400_000,
            BlockKind::Stretch,
            120,
            Exercise::OnClickQuarters,
            60,
            30_000,
        ),
        &grades(Grade::Good),
    );
    let before = snapshot(&l);
    let firsts = l.firsts.clone();

    let old = r#"{"v":1,"strums":1,"blocks":0,"active_ms":0,"days":0,"best_bpm":{"quarters":20},"firsts":[]}"#;
    l.merge(&DisplayLedger::from_json(old).unwrap());
    assert_eq!(snapshot(&l), before);

    let tampered = r#"{"strums":-5,"blocks":"x","active_ms":null,"days":1.5,"best_bpm":{"quarters":-1},"firsts":7,"seen_days":"no"}"#;
    l.merge(&DisplayLedger::from_json(tampered).unwrap());
    assert_eq!(snapshot(&l), before);
    assert_eq!(l.firsts, firsts);

    l.merge(&DisplayLedger::new());
    assert_eq!(snapshot(&l), before);
    assert!(DisplayLedger::from_json("not json").is_none());
}

// (3) dispute changes nothing; disputed NotHeard adds no strum
#[test]
fn dispute_changes_nothing_and_adds_no_strum() {
    let mut l = DisplayLedger::new();
    // Block with zero heard attacks whose only evidence is a disputed NotHeard.
    let mut b = block(
        1000,
        BlockKind::Familiar,
        60,
        Exercise::OnClickQuarters,
        0,
        20_000,
    );
    b.evidence[0].outcome = Outcome::Zero(ZeroWhy::Disputed);
    l.apply(&b, &grades(Grade::NoReading));
    assert_eq!(l.strums.get(), 0);
    assert_eq!(l.blocks.get(), 1);
    assert_eq!(l.active_ms.get(), 20_000);
    let before = l.clone();
    l.on_dispute(1000, 0);
    assert_eq!(l, before);
}

// (4) J3: probe adds totals only
#[test]
fn probe_adds_totals_but_no_mark_or_milestone() {
    let mut l = DisplayLedger::new();
    l.apply(
        &block(
            5 * 86_400_000,
            BlockKind::Probe,
            140,
            Exercise::OnClickEighths,
            40,
            50_000,
        ),
        &grades(Grade::Good),
    );
    assert_eq!(l.strums.get(), 40);
    assert_eq!(l.blocks.get(), 1);
    assert_eq!(l.active_ms.get(), 50_000);
    assert_eq!(l.days.get(), 1);
    assert!(l.best_bpm.is_empty());
    assert_eq!(l.best_block_strums.get(), 0);
    assert!(l.firsts.is_empty());
    // A normal block does create them.
    l.apply(
        &block(
            6 * 86_400_000,
            BlockKind::Stretch,
            100,
            Exercise::OnClickEighths,
            10,
            1000,
        ),
        &grades(Grade::Good),
    );
    assert_eq!(l.best_bpm["eighths"].get(), 100);
    assert!(l.firsts.contains("first_good_block"));
    assert_eq!(l.days.get(), 2);
}

// Weak never removes anything; days count distinct days, not streaks.
#[test]
fn weak_removes_nothing_and_days_are_distinct() {
    let mut l = DisplayLedger::new();
    l.apply(
        &block(
            0,
            BlockKind::Stretch,
            100,
            Exercise::OnClickQuarters,
            10,
            1000,
        ),
        &grades(Grade::Good),
    );
    let before = (l.best_bpm.clone(), l.firsts.clone());
    l.apply(
        &block(
            1000,
            BlockKind::Recover,
            60,
            Exercise::OnClickQuarters,
            10,
            1000,
        ),
        &grades(Grade::Weak),
    );
    assert_eq!((l.best_bpm.clone(), l.firsts.clone()), before);
    assert_eq!(l.days.get(), 1);
}

// (5) Mono surface: private field, no set / Sub / From<u64>.
#[test]
fn mono_has_no_way_down() {
    let src = include_str!("../src/display.rs");
    let body = src.split("fn exercise_name").next().unwrap();
    assert!(
        body.contains("pub struct Mono(u64);"),
        "field must be private"
    );
    for banned in [
        "fn set",
        "impl Sub",
        "impl SubAssign",
        "impl From<u64>",
        "impl std::ops",
        "pub u64",
        "DerefMut",
    ] {
        assert!(!body.contains(banned), "Mono must not contain `{banned}`");
    }
    let mut m = Mono::default();
    m.add(5);
    m.raise(3);
    assert_eq!(m.get(), 5);
    m.raise(9);
    assert_eq!(m.get(), 9);
    m.add(u64::MAX);
    assert_eq!(m.get(), u64::MAX);
}
