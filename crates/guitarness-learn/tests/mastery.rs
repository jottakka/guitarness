//! N1 tests: evidence table, zero-weight neutrality, weights, decay, grade, fold, dispute, determinism.

use guitarness_core::chart::{Chart, Dir, EventKind};
use guitarness_core::gates::{GateState, GateTable};
use guitarness_core::pulse::{Exercise, exercise};
use guitarness_core::verdict::{
    Attack, Direction, JudgeOut, PulseVerdict, Timing, VerdictType, Why,
};
use guitarness_learn::evidence::{evidence_of_round, skill_of, weight};
use guitarness_learn::mastery::{DAY_MS, HOUR_MS, LearnerLedger, grade_of};
use guitarness_learn::types::{
    BlockKind, BlockRecord, BlockSpec, Evidence, Grade, LearnConfig, LogLine, Outcome, Provenance,
    SkillKey, ZeroWhy,
};

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
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
}

fn cfg() -> LearnConfig {
    LearnConfig::default()
}

fn key(s: &str) -> SkillKey {
    SkillKey(s.to_string())
}

fn verdict(id: u32, attack: Attack, timing: Timing, fin: bool) -> PulseVerdict {
    PulseVerdict {
        id,
        attack,
        timing,
        dt_ms: matches!(timing, Timing::Center | Timing::InWindow | Timing::Outside).then_some(4.0),
        dir: Direction::NotJudged(Why::GateLocked),
        onset_in: None,
        pos_in_bar: 0,
        fin,
    }
}

fn v(id: u32, attack: Attack, timing: Timing) -> JudgeOut {
    JudgeOut::Verdict(verdict(id, attack, timing, true))
}

fn prov() -> Provenance {
    Provenance {
        judge_v: "test".into(),
        basis: "estimate".into(),
        gate: GateState::Provisional,
        tape: None,
    }
}

fn spec(bpm: u16) -> BlockSpec {
    BlockSpec {
        exercise: Exercise::OnClickQuarters,
        bpm,
        bars: 4,
    }
}

fn ev(id: u32, skill: &str, outcome: Outcome, gate: GateState) -> Evidence {
    Evidence {
        id,
        skill: key(skill),
        vtype: VerdictType::Timing,
        outcome,
        gate,
        dt_ms: None,
    }
}

fn block(ts_ms: i64, evidence: Vec<Evidence>) -> BlockRecord {
    BlockRecord {
        ts_ms,
        dur_ms: 30_000,
        session: 1,
        kind: BlockKind::Familiar,
        spec: spec(60),
        after_gap: false,
        strums: 0,
        evidence,
        prov: prov(),
    }
}

const SKILLS: [&str; 3] = ["pulse:quarter", "pulse:eighth-down", "pulse:eighth-up"];

fn random_outcome(r: &mut Rng) -> Outcome {
    match r.below(10) {
        0..=5 => Outcome::Success,
        6..=7 => Outcome::Failure,
        8 => Outcome::Zero(ZeroWhy::NotHeard),
        _ => Outcome::Zero(ZeroWhy::NotJudged(Why::GateLocked)),
    }
}

fn random_log(r: &mut Rng, blocks: usize) -> Vec<BlockRecord> {
    let mut t = r.range(0, 1000) * HOUR_MS;
    let mut out = Vec::new();
    for _ in 0..blocks {
        t += r.range(1, 40 * 24) * HOUR_MS;
        let n = r.range(0, 30);
        let evs = (0..n)
            .map(|i| {
                let gate = if r.below(3) == 0 {
                    GateState::Provisional
                } else {
                    GateState::Open
                };
                ev(
                    i as u32,
                    SKILLS[r.below(3) as usize],
                    random_outcome(r),
                    gate,
                )
            })
            .collect();
        let mut b = block(t, evs);
        b.after_gap = r.below(4) == 0;
        b.spec = spec(r.range(40, 120) as u16);
        out.push(b);
    }
    out
}

fn fold_blocks(bs: &[BlockRecord]) -> LearnerLedger {
    let log: Vec<LogLine> = bs.iter().cloned().map(LogLine::Block).collect();
    LearnerLedger::fold(&log, &cfg())
}

// (1) Evidence table, case by case.
#[test]
fn evidence_table() {
    let mut chart = exercise(Exercise::OnClickEighths, 100.0, 1);
    chart.tiers[0].events[3].kind = EventKind::Ghost { dir: Dir::Up };
    let gates = GateTable::builtin();
    let gate = gates.state(VerdictType::Timing);
    let outs = vec![
        JudgeOut::Flash {
            id: Some(0),
            dt_ms: None,
        },
        // Non-final verdict for id 0, then the final one: the final wins.
        JudgeOut::Verdict(verdict(0, Attack::Heard, Timing::Outside, false)),
        v(0, Attack::Heard, Timing::Center),
        v(1, Attack::Heard, Timing::InWindow),
        v(2, Attack::Heard, Timing::Outside),
        v(
            3,
            Attack::NotJudged(Why::Ghost),
            Timing::NotJudged(Why::Ghost),
        ),
        v(4, Attack::NotHeard, Timing::NotJudged(Why::NoAttack)),
        v(
            5,
            Attack::NotJudged(Why::ClockNotLocked),
            Timing::NotJudged(Why::ClockNotLocked),
        ),
        v(6, Attack::Heard, Timing::NotJudged(Why::NoCalibration)),
        v(7, Attack::Heard, Timing::Center),
        JudgeOut::Withdrawn {
            id: 7,
            why: Why::Disputed,
        },
        JudgeOut::Extra { t_in: 1.0 },
    ];
    let (evs, strums) = evidence_of_round(&chart, &outs, &gates);
    let ids: Vec<u32> = evs.iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        vec![0, 1, 2, 4, 5, 6, 7],
        "ghost and Extra give no line"
    );
    let by = |id: u32| evs.iter().find(|e| e.id == id).unwrap();
    assert_eq!(by(0).outcome, Outcome::Success);
    assert_eq!(by(0).skill, key("pulse:eighth-down"));
    assert_eq!(by(0).dt_ms, Some(4.0));
    assert_eq!(by(1).outcome, Outcome::Success);
    assert_eq!(by(1).skill, key("pulse:eighth-up"));
    assert_eq!(by(2).outcome, Outcome::Failure);
    assert_eq!(by(4).outcome, Outcome::Zero(ZeroWhy::NotHeard));
    assert_eq!(by(4).vtype, VerdictType::Attack);
    assert_eq!(
        by(5).outcome,
        Outcome::Zero(ZeroWhy::NotJudged(Why::ClockNotLocked))
    );
    assert_eq!(
        by(6).outcome,
        Outcome::Zero(ZeroWhy::NotJudged(Why::NoCalibration))
    );
    assert_eq!(by(7).outcome, Outcome::Zero(ZeroWhy::Disputed));
    assert_eq!(by(7).dt_ms, None);
    assert!(evs.iter().all(|e| e.gate == gate));
    // Heard attacks that were not withdrawn: ids 0, 1, 2, 6.
    assert_eq!(strums, 4);
    // A withdrawn non-dispute reason keeps its reason.
    let outs2 = vec![
        v(0, Attack::Heard, Timing::Center),
        JudgeOut::Withdrawn {
            id: 0,
            why: Why::Glitch,
        },
    ];
    let (e2, s2) = evidence_of_round(&chart, &outs2, &gates);
    assert_eq!(e2.len(), 1);
    assert_eq!(
        e2[0].outcome,
        Outcome::Zero(ZeroWhy::NotJudged(Why::Glitch))
    );
    assert_eq!(s2, 0);
    // Skill keys.
    let q = exercise(Exercise::OnClickQuarters, 100.0, 1);
    assert_eq!(skill_of(&q, 0), key("pulse:quarter"));
    assert_eq!(skill_of(&Chart::new("other", 90.0), 3), key("pulse:free"));
}

fn bits(l: &LearnerLedger) -> Vec<(String, u64, u64, i64)> {
    l.skills
        .iter()
        .map(|(k, s)| (k.0.clone(), s.a.to_bits(), s.b.to_bits(), s.last_ms))
        .collect()
}

// (2) J1: zero-weight evidence is a bit-exact no-op.
#[test]
fn zero_weight_is_a_noop() {
    let mut r = Rng(0xA11CE);
    for _ in 0..1000 {
        let base = random_log(&mut r, 6);
        let mut with = base.clone();
        let inserts = r.range(1, 50);
        for _ in 0..inserts {
            let bi = r.below(with.len() as u64) as usize;
            let pos = r.below(with[bi].evidence.len() as u64 + 1) as usize;
            let skill = if r.below(2) == 0 {
                SKILLS[r.below(3) as usize]
            } else {
                "pulse:never-seen"
            };
            let outcome = match r.below(3) {
                0 => Outcome::Zero(ZeroWhy::NotHeard),
                1 => Outcome::Zero(ZeroWhy::Disputed),
                _ => Outcome::Zero(ZeroWhy::NotJudged(Why::Glitch)),
            };
            with[bi].evidence.insert(
                pos,
                ev(900 + r.below(50) as u32, skill, outcome, GateState::Open),
            );
        }
        // A Success on a Locked gate also weighs zero.
        let la = fold_blocks(&base);
        let lb = fold_blocks(&with);
        assert_eq!(bits(&la), bits(&lb));
        assert_eq!(la.skills, lb.skills);
        assert_eq!(la.last_good, lb.last_good);
        assert_eq!(la.last_block_end_ms, lb.last_block_end_ms);
    }
    let c = cfg();
    assert_eq!(
        weight(
            &ev(0, "pulse:quarter", Outcome::Success, GateState::Locked),
            &c
        ),
        0.0
    );
}

// (3) Weights: Open 1, Provisional 0.5, variant (b) 0.
#[test]
fn provisional_weights() {
    let c = cfg();
    let k = key("pulse:quarter");
    let open = ev(0, "pulse:quarter", Outcome::Success, GateState::Open);
    let prov_s = ev(0, "pulse:quarter", Outcome::Success, GateState::Provisional);
    let prov_f = ev(0, "pulse:quarter", Outcome::Failure, GateState::Provisional);
    assert_eq!(weight(&open, &c), 1.0);
    assert_eq!(weight(&prov_s, &c), 0.5);
    let mut l = LearnerLedger::new(&c);
    l.apply(
        &block(0, vec![prov_s.clone(), prov_f.clone(), open.clone()]),
        &c,
    );
    let st = &l.skills[&k];
    assert_eq!(st.a, 1.5);
    assert_eq!(st.b, 0.5);
    let c0 = LearnConfig {
        provisional_weight: 0.0,
        ..cfg()
    };
    let mut l0 = LearnerLedger::new(&c0);
    l0.apply(&block(0, vec![prov_s, prov_f]), &c0);
    assert!(l0.skills.is_empty(), "variant (b): nothing is added");
    assert_eq!(l0.mean(&k), 0.5);
}

// (4) Decay.
#[test]
fn decay_over_fourteen_days() {
    let c = cfg();
    let k = key("pulse:quarter");
    let mut l = LearnerLedger::new(&c);
    let evs = (0..5)
        .map(|i| ev(i, "pulse:quarter", Outcome::Success, GateState::Open))
        .chain((5..7).map(|i| ev(i, "pulse:quarter", Outcome::Failure, GateState::Open)))
        .collect();
    l.apply(&block(0, evs), &c);
    let (a0, b0) = (l.skills[&k].a, l.skills[&k].b);
    assert_eq!((a0, b0), (5.0, 2.0));
    let e1 = (-1.0f64).exp();
    let (a, b) = l.decayed(&k, 14 * DAY_MS, &c);
    assert!((a - a0 * e1).abs() < 1e-12 && (b - b0 * e1).abs() < 1e-12);
    // Applying a later block decays the stored state the same way.
    l.apply(
        &block(
            14 * DAY_MS,
            vec![ev(0, "pulse:quarter", Outcome::Success, GateState::Open)],
        ),
        &c,
    );
    assert!((l.skills[&k].a - (a0 * e1 + 1.0)).abs() < 1e-12);
    assert!((l.skills[&k].b - b0 * e1).abs() < 1e-12);
    // Toward the prior, never past it.
    let mut l = LearnerLedger::new(&c);
    let evs: Vec<Evidence> = (0..9)
        .map(|i| ev(i, "pulse:quarter", Outcome::Success, GateState::Open))
        .collect();
    l.apply(&block(0, evs), &c);
    let prior_mean = c.prior.0 / (c.prior.0 + c.prior.1);
    let mut prev = l.mean_at(&k, 0, &c);
    assert!(prev > prior_mean);
    for d in [1, 3, 7, 14, 30, 90, 365, 3650] {
        let m = l.mean_at(&k, d * DAY_MS, &c);
        assert!(m <= prev + 1e-15 && m >= prior_mean, "day {d}: {m}");
        prev = m;
    }
    assert!((prev - prior_mean).abs() < 1e-12);
    println!("decay: e^-1 check within 1e-12 on (a, b); mean at 10y = {prev}");
}

// (5) Grade boundaries.
#[test]
fn grade_boundaries() {
    let c = cfg();
    assert_eq!(grade_of(7.99, 0.0, &c), Grade::NoReading);
    assert_eq!(grade_of(3.99, 4.0, &c), Grade::NoReading);
    assert_eq!(grade_of(8.0, 0.0, &c), Grade::Good);
    assert_eq!(grade_of(8.0, 2.0, &c), Grade::Good, "0.80 is Good");
    assert_eq!(grade_of(7.9, 2.1, &c), Grade::Fair);
    assert_eq!(grade_of(6.0, 4.0, &c), Grade::Fair, "0.60 is Fair");
    assert_eq!(grade_of(5.9, 4.1, &c), Grade::Weak);
    assert_eq!(grade_of(0.0, 8.0, &c), Grade::Weak);
    // Through apply: 15 provisional successes are n = 7.5, 16 are n = 8.
    let k = key("pulse:quarter");
    let mk = |n: u32| -> Vec<Evidence> {
        (0..n)
            .map(|i| ev(i, "pulse:quarter", Outcome::Success, GateState::Provisional))
            .collect()
    };
    let mut l = LearnerLedger::new(&c);
    assert_eq!(
        l.apply(&block(0, mk(15)), &c),
        vec![(k.clone(), Grade::NoReading)]
    );
    assert_eq!(l.apply(&block(1000, mk(16)), &c), vec![(k, Grade::Good)]);
}

// (6) Incremental apply == fold.
#[test]
fn apply_equals_fold() {
    let mut r = Rng(0xF01D);
    let c = cfg();
    for _ in 0..1000 {
        let bs = random_log(&mut r, 8);
        let mut log: Vec<LogLine> = Vec::new();
        let mut inc = LearnerLedger::new(&c);
        for b in &bs {
            log.push(LogLine::Block(b.clone()));
            log.push(LogLine::Choice(guitarness_learn::types::Choice::Accepted));
            inc.apply(b, &c);
        }
        assert_eq!(inc, LearnerLedger::fold(&log, &c));
    }
}

// (7) A block with only NotHeard: no reading, domain and agenda untouched.
#[test]
fn only_not_heard_is_no_reading() {
    let c = cfg();
    let k = key("pulse:quarter");
    let mut l = LearnerLedger::new(&c);
    let good: Vec<Evidence> = (0..10)
        .map(|i| ev(i, "pulse:quarter", Outcome::Success, GateState::Open))
        .collect();
    l.apply(&block(0, good), &c);
    let before = l.skills.clone();
    let none: Vec<Evidence> = (0..12)
        .map(|i| {
            ev(
                i,
                "pulse:quarter",
                Outcome::Zero(ZeroWhy::NotHeard),
                GateState::Open,
            )
        })
        .collect();
    let g = l.apply(&block(5 * DAY_MS, none), &c);
    assert_eq!(g, vec![(k, Grade::NoReading)]);
    assert_eq!(l.skills, before);
    // Empty block: also nothing.
    assert!(l.apply(&block(6 * DAY_MS, vec![]), &c).is_empty());
    assert_eq!(l.skills, before);
}

// (8) Dispute: fold(with i disputed) == fold(without i); never inflates mastery.
#[test]
fn dispute_is_erasure_and_never_inflates() {
    let mut r = Rng(0xD15B07E);
    let c = cfg();
    let mut checked = 0;
    let mut worst = 0.0f64;
    for _ in 0..200 {
        let bs = random_log(&mut r, 10);
        // Pick a weighted evidence line.
        let cands: Vec<(usize, usize)> = bs
            .iter()
            .enumerate()
            .flat_map(|(bi, b)| {
                b.evidence
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| weight(e, &c) > 0.0)
                    .map(move |(ei, _)| (bi, ei))
            })
            .collect();
        if cands.is_empty() {
            continue;
        }
        let (bi, ei) = cands[r.below(cands.len() as u64) as usize];
        let (ts, id) = (bs[bi].ts_ms, bs[bi].evidence[ei].id);
        // Make ids unique inside the block so the dispute names one line.
        let mut bs = bs;
        for b in &mut bs {
            for (i, e) in b.evidence.iter_mut().enumerate() {
                e.id = i as u32;
            }
        }
        let id = if bs[bi].evidence[ei].id == id {
            id
        } else {
            ei as u32
        };
        let mut log: Vec<LogLine> = bs.iter().cloned().map(LogLine::Block).collect();
        log.push(LogLine::Dispute { block_ts: ts, id });
        let mut removed = bs.clone();
        removed[bi].evidence.remove(ei);
        assert_eq!(LearnerLedger::fold(&log, &c), fold_blocks(&removed));
        // Never inflates: compare with "counts as a hit" at every later instant.
        let mut hit = bs.clone();
        hit[bi].evidence[ei].outcome = Outcome::Success;
        let mut a = LearnerLedger::new(&c);
        let mut h = LearnerLedger::new(&c);
        for j in 0..bs.len() {
            let mut bj = bs[j].clone();
            if j == bi {
                bj.evidence[ei].outcome = Outcome::Zero(ZeroWhy::Disputed);
            }
            a.apply(&bj, &c);
            h.apply(&hit[j], &c);
            if j < bi {
                continue;
            }
            let t = bs[j].ts_ms;
            for s in SKILLS {
                let k = key(s);
                let (pa, ph) = (a.mean_at(&k, t, &c), h.mean_at(&k, t, &c));
                let (na, nh) = (a.n_ef_at(&k, t, &c), h.n_ef_at(&k, t, &c));
                assert!(pa <= ph + 1e-9, "p_hat inflated: {pa} > {ph}");
                assert!(na <= nh + 1e-9, "n_ef inflated: {na} > {nh}");
                worst = worst.max(pa - ph).max(na - nh);
            }
        }
        checked += 1;
    }
    assert!(checked >= 190);
    println!("dispute: {checked} logs checked; max (disputed - hit) = {worst:e} (<= 0 expected)");
}

// (9) Determinism.
#[test]
fn deterministic() {
    let mut r = Rng(7);
    let bs = random_log(&mut r, 12);
    assert_eq!(fold_blocks(&bs), fold_blocks(&bs));
    let chart = exercise(Exercise::OnClickEighths, 100.0, 2);
    let outs: Vec<JudgeOut> = (0..16)
        .map(|i| {
            if i % 3 == 0 {
                v(i, Attack::NotHeard, Timing::NotJudged(Why::NoAttack))
            } else {
                v(i, Attack::Heard, Timing::Center)
            }
        })
        .collect();
    let g = GateTable::builtin();
    assert_eq!(
        evidence_of_round(&chart, &outs, &g),
        evidence_of_round(&chart, &outs, &g)
    );
}

// Agenda (§2.3), driven through apply.
#[test]
fn agenda_rules() {
    let c = cfg();
    let k = key("pulse:quarter");
    let good = |t: i64, bpm: u16, gap: bool| {
        let evs: Vec<Evidence> = (0..10)
            .map(|i| ev(i, "pulse:quarter", Outcome::Success, GateState::Open))
            .collect();
        let mut b = block(t, evs);
        b.spec = spec(bpm);
        b.after_gap = gap;
        b
    };
    let weak = |t: i64, gap: bool| {
        let evs: Vec<Evidence> = (0..10)
            .map(|i| ev(i, "pulse:quarter", Outcome::Failure, GateState::Open))
            .collect();
        let mut b = block(t, evs);
        b.after_gap = gap;
        b
    };
    let mut l = LearnerLedger::new(&c);
    l.apply(&good(0, 60, false), &c);
    assert_eq!(l.skills[&k].step, Some(0));
    assert_eq!(l.skills[&k].due_ms, DAY_MS);
    // Same day: only comfort moves.
    l.apply(&good(2 * HOUR_MS, 70, false), &c);
    assert_eq!(l.skills[&k].step, Some(0));
    assert_eq!(l.skills[&k].comfort.unwrap().bpm, 70);
    // Other day: step up, capped at 4.
    for d in 1..=8 {
        l.apply(&good(d * DAY_MS, 70, false), &c);
    }
    assert_eq!(l.skills[&k].step, Some(4));
    assert_eq!(l.skills[&k].due_ms, 8 * DAY_MS + 16 * DAY_MS);
    // Weak outside a gap: agenda still.
    let before = l.skills[&k].clone();
    l.apply(&weak(10 * DAY_MS, false), &c);
    assert_eq!(l.skills[&k].step, before.step);
    assert_eq!(l.skills[&k].due_ms, before.due_ms);
    assert_eq!(l.skills[&k].last_grade, Grade::Weak);
    // Weak after a gap: one step down, never below 0.
    l.apply(&weak(20 * DAY_MS, true), &c);
    assert_eq!(l.skills[&k].step, Some(3));
    for i in 0..10 {
        l.apply(&weak((30 + i) * DAY_MS, true), &c);
    }
    assert_eq!(l.skills[&k].step, Some(0));
    // The free-play skill never enters the agenda.
    let mut l = LearnerLedger::new(&c);
    let evs: Vec<Evidence> = (0..10)
        .map(|i| ev(i, "pulse:free", Outcome::Success, GateState::Open))
        .collect();
    l.apply(&block(0, evs), &c);
    assert_eq!(l.skills[&key("pulse:free")].step, None);
}
