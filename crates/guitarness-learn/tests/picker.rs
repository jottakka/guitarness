//! N3 tests: agenda read through the picker, one axis, rule order, probe, stretch gate,
//! exploration probabilities, no public due list.

use guitarness_core::gates::GateState;
use guitarness_core::pulse::Exercise;
use guitarness_core::verdict::VerdictType;
use guitarness_learn::mastery::{DAY_MS, HOUR_MS, LearnerLedger, STEPS};
use guitarness_learn::picker::{next_offer, something_else, something_else_in};
use guitarness_learn::probe::{choice_for_else, cut_to_probe};
use guitarness_learn::types::{
    Axis, BlockKind, BlockRecord, BlockSpec, Choice, Evidence, Grade, LearnConfig, Offer, Outcome,
    Provenance, SessionCtx, SkillKey,
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
fn q(bpm: u16) -> BlockSpec {
    BlockSpec {
        exercise: Exercise::OnClickQuarters,
        bpm,
        bars: 4,
    }
}
fn e(bpm: u16) -> BlockSpec {
    BlockSpec {
        exercise: Exercise::OnClickEighths,
        bpm,
        bars: 4,
    }
}

/// Block with `s` successes and `f` failures on `skill` (all `Open`, weight 1).
fn blk(ts: i64, skill: &str, spec: BlockSpec, s: u32, f: u32, after_gap: bool) -> BlockRecord {
    let mut evidence = Vec::new();
    for i in 0..(s + f) {
        evidence.push(Evidence {
            id: i,
            skill: key(skill),
            vtype: VerdictType::Timing,
            outcome: if i < s {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            gate: GateState::Open,
            dt_ms: None,
        });
    }
    BlockRecord {
        ts_ms: ts,
        dur_ms: 30_000,
        session: 1,
        kind: BlockKind::Familiar,
        spec,
        after_gap,
        strums: s + f,
        evidence,
        prov: Provenance {
            judge_v: "t".into(),
            basis: "estimate".into(),
            gate: GateState::Open,
            tape: None,
        },
    }
}

fn good(ts: i64, skill: &str, spec: BlockSpec) -> BlockRecord {
    blk(ts, skill, spec, 10, 0, false)
}
fn weak(ts: i64, skill: &str, spec: BlockSpec, after_gap: bool) -> BlockRecord {
    blk(ts, skill, spec, 0, 10, after_gap)
}
fn fair(ts: i64, skill: &str, spec: BlockSpec) -> BlockRecord {
    blk(ts, skill, spec, 7, 3, false)
}
fn empty(ts: i64, spec: BlockSpec) -> BlockRecord {
    blk(ts, "pulse:quarter", spec, 0, 0, false)
}

fn ctx(now_ms: i64, blocks: u32, probe_done: bool, offer_no: u64) -> SessionCtx {
    SessionCtx {
        now_ms,
        blocks_this_session: blocks,
        probe_done,
        offer_no,
    }
}

fn ledger(bs: &[BlockRecord]) -> LearnerLedger {
    let mut l = LearnerLedger::new(&cfg());
    for b in bs {
        l.apply(b, &cfg());
    }
    l
}

fn kinds(o: &Offer) -> Vec<BlockKind> {
    o.cands.iter().map(|c| c.plan.kind).collect()
}

/// A mid-session context: no probe, not the first block.
fn has_review(l: &LearnerLedger, now: i64) -> Option<BlockSpec> {
    let o = next_offer(l, &ctx(now, 2, true, 0), &cfg());
    o.cands
        .iter()
        .find(|c| c.plan.kind == BlockKind::Review)
        .map(|c| c.plan.spec)
}

const T0: i64 = 1_000 * HOUR_MS;

// ---------- 1. agenda rules, read through the picker ----------

#[test]
fn agenda_rules_hold() {
    // First Good enters step 0, due = t + 1 day, comfort = block spec.
    let mut l = ledger(&[good(T0, "pulse:quarter", q(60))]);
    let st = &l.skills[&key("pulse:quarter")];
    assert_eq!((st.step, st.due_ms), (Some(0), T0 + STEPS[0] * DAY_MS));
    assert_eq!(has_review(&l, T0 + DAY_MS - 1), None);
    assert_eq!(has_review(&l, T0 + DAY_MS), Some(q(60)));

    // Good the same day: only comfort moves; due and step stay.
    l.apply(&good(T0 + 2 * HOUR_MS, "pulse:quarter", q(70)), &cfg());
    let st = &l.skills[&key("pulse:quarter")];
    assert_eq!((st.step, st.due_ms), (Some(0), T0 + DAY_MS));
    assert_eq!(has_review(&l, T0 + DAY_MS), Some(q(70)));

    // Good on another day (>= 20 h after the last step-up): step climbs to 4 and stays there.
    let mut t = T0;
    let mut want = 0usize;
    for i in 0..9 {
        t += 21 * HOUR_MS;
        l.apply(&good(t, "pulse:quarter", q(70 + i)), &cfg());
        want = (want + 1).min(4);
        let st = &l.skills[&key("pulse:quarter")];
        assert_eq!(st.step, Some(want as u8));
        let due = t + STEPS[want] * DAY_MS;
        assert_eq!(st.due_ms, due);
        assert_eq!(has_review(&l, due - 1), None, "step {want}");
        assert_eq!(has_review(&l, due), Some(q(70 + i)), "step {want}");
    }
    assert_eq!(want, 4);

    // Weak after a pause: one step down, due = t + STEPS[step]; floor at 0.
    let mut t2 = t;
    for expect in [3usize, 2, 1, 0, 0, 0] {
        t2 += 60 * HOUR_MS;
        l.apply(&weak(t2, "pulse:quarter", q(70), true), &cfg());
        let st = &l.skills[&key("pulse:quarter")];
        assert_eq!(st.step, Some(expect as u8));
        assert_eq!(st.due_ms, t2 + STEPS[expect] * DAY_MS);
    }

    // Weak outside a pause, Fair, and NoReading do not move the agenda.
    let before = l.skills[&key("pulse:quarter")].clone();
    t2 += 3 * HOUR_MS;
    l.apply(&weak(t2, "pulse:quarter", q(70), false), &cfg());
    t2 += 3 * HOUR_MS;
    l.apply(&fair(t2, "pulse:quarter", q(70)), &cfg());
    t2 += 3 * HOUR_MS;
    l.apply(&empty(t2, q(70)), &cfg());
    let after = &l.skills[&key("pulse:quarter")];
    assert_eq!((after.step, after.due_ms), (before.step, before.due_ms));
    assert_eq!(after.comfort, before.comfort);
    assert_eq!(has_review(&l, before.due_ms - 1), None);
    assert!(has_review(&l, before.due_ms).is_some());

    // Random sequences: step stays in 0..=4.
    let mut r = Rng(7);
    for _ in 0..300 {
        let mut l = LearnerLedger::new(&cfg());
        let mut t = T0;
        for _ in 0..r.range(1, 25) {
            t += r.range(1, 100) * HOUR_MS;
            let b = match r.below(3) {
                0 => good(t, "pulse:quarter", q(60)),
                1 => weak(t, "pulse:quarter", q(60), r.below(2) == 0),
                _ => fair(t, "pulse:quarter", q(60)),
            };
            l.apply(&b, &cfg());
            if let Some(s) = l.skills.get(&key("pulse:quarter")).and_then(|s| s.step) {
                assert!(s <= 4);
            }
        }
    }
}

// ---------- 2. J4: one offer, at most one axis against the skill's own comfort ----------

fn base_of(l: &LearnerLedger, k: &SkillKey) -> BlockSpec {
    if let Some(c) = l.skills.get(k).and_then(|s| s.comfort) {
        return c;
    }
    let ex = match k.0.as_str() {
        "pulse:quarter" => Exercise::OnClickQuarters,
        "pulse:eighth-down" | "pulse:eighth-up" => Exercise::OnClickEighths,
        _ => cfg().starter.exercise,
    };
    BlockSpec {
        exercise: ex,
        ..cfg().starter
    }
}

fn axes(a: &BlockSpec, b: &BlockSpec) -> u32 {
    u32::from(a.exercise != b.exercise) + u32::from(a.bpm != b.bpm)
}

fn random_ledger(r: &mut Rng) -> (LearnerLedger, i64) {
    let skills = [
        "pulse:quarter",
        "pulse:eighth-down",
        "pulse:eighth-up",
        "pulse:free",
    ];
    let mut l = LearnerLedger::new(&cfg());
    let mut t = T0;
    for _ in 0..r.range(0, 6) {
        t += r.range(1, 100) * HOUR_MS;
        let spec = BlockSpec {
            exercise: if r.below(2) == 0 {
                Exercise::OnClickQuarters
            } else {
                Exercise::OnClickEighths
            },
            bpm: r.range(20, 240) as u16,
            bars: r.range(1, 8) as u8,
        };
        let s = r.range(0, 45) as u32;
        let f = r.range(0, 12) as u32;
        let sk = skills[r.below(4) as usize];
        l.apply(&blk(t, sk, spec, s, f, r.below(3) == 0), &cfg());
    }
    (l, t + 30_000)
}

#[test]
fn one_offer_at_most_one_axis() {
    let mut r = Rng(42);
    let c = cfg();
    for n in 0..10_000u64 {
        let (l, end) = random_ledger(&mut r);
        let now = end + r.range(0, 60 * 24) * HOUR_MS;
        let cx = ctx(now, r.below(4) as u32, r.below(2) == 0, n);
        let o = next_offer(&l, &cx, &c);
        assert!(!o.cands.is_empty() && o.chosen < o.cands.len());
        let sum: f64 = o.cands.iter().map(|c| c.p).sum();
        assert!((sum - 1.0).abs() < 1e-12);
        for cand in &o.cands {
            let p = &cand.plan;
            assert!((20..=240).contains(&p.spec.bpm) && p.spec.bars >= 1);
            assert_eq!(p.show_summary, p.kind != BlockKind::Probe);
            if p.kind == BlockKind::Probe {
                let g = l.last_good.unwrap();
                assert_eq!((p.spec.exercise, p.spec.bpm), (g.exercise, g.bpm));
                continue;
            }
            let base = base_of(&l, &p.skill);
            assert!(axes(&base, &p.spec) <= 1, "{:?} vs {:?}", base, p);
        }
        // Reported change matches the chosen plan and names one axis.
        let ch = &o.cands[o.chosen];
        match o.change {
            None => {}
            Some((Axis::Tempo, old, new)) => {
                assert_eq!(new, ch.plan.spec.bpm);
                // Never a downward change; Recover and Free carry none.
                assert!(new > old);
                assert!(!matches!(
                    ch.plan.kind,
                    BlockKind::Recover | BlockKind::Free
                ));
            }
            Some((Axis::Rhythm, old, new)) => {
                assert_ne!(old, new);
                assert_eq!(ch.plan.kind, BlockKind::Stretch);
            }
        }
        assert!(o.change.is_none() || ch.plan.kind != BlockKind::Probe);
        // Same inputs, same offer.
        assert_eq!(o, next_offer(&l, &cx, &c));
    }
}

// ---------- 3. rule order ----------

#[test]
fn rules_win_in_order() {
    let c = cfg();
    let first_kind = |l: &LearnerLedger, x: SessionCtx| next_offer(l, &x, &c).cands[0].plan.kind;

    // No history: Familiar (starter) first, Free afterwards.
    let l0 = LearnerLedger::new(&c);
    let o = next_offer(&l0, &ctx(T0, 0, false, 0), &c);
    assert_eq!(o.cands[0].plan.kind, BlockKind::Familiar);
    assert_eq!(o.cands[0].plan.spec, c.starter);
    assert_eq!(first_kind(&l0, ctx(T0, 1, false, 0)), BlockKind::Free);

    // Stretch-eligible skill (40 successes), a Good block at T0, due tomorrow.
    let strong = ledger(&[blk(T0, "pulse:quarter", q(100), 40, 0, false)]);
    let soon = T0 + HOUR_MS;
    assert_eq!(
        first_kind(&strong, ctx(soon, 1, false, 0)),
        BlockKind::Stretch
    );
    // First block of the session beats Stretch.
    assert_eq!(
        first_kind(&strong, ctx(soon, 0, false, 0)),
        BlockKind::Familiar
    );
    // Due beats Stretch.
    let due = T0 + DAY_MS;
    assert_eq!(
        first_kind(&strong, ctx(due, 1, false, 0)),
        BlockKind::Review
    );
    // Weak last block beats Review (and Stretch); Familiar beats Recover on a first block.
    let mut weakl = strong.clone();
    weakl.apply(&weak(T0 + 2 * DAY_MS, "pulse:quarter", q(100), false), &c);
    let now = T0 + 2 * DAY_MS + HOUR_MS;
    assert!(weakl.skills[&key("pulse:quarter")].due_ms <= now);
    assert_eq!(
        first_kind(&weakl, ctx(now, 1, false, 0)),
        BlockKind::Recover
    );
    assert_eq!(
        first_kind(&weakl, ctx(now, 0, false, 0)),
        BlockKind::Familiar
    );
    // Probe beats Familiar after 48 h (and needs a Good block).
    let later = T0 + 30_000 + 48 * HOUR_MS;
    assert_eq!(
        first_kind(&strong, ctx(later, 0, false, 0)),
        BlockKind::Probe
    );
    assert_eq!(
        first_kind(&strong, ctx(later, 0, true, 0)),
        BlockKind::Familiar
    );
    // Recover sits at the right place when the last block is Weak and nothing is due.
    let wk = ledger(&[weak(T0, "pulse:quarter", q(100), false)]);
    let o = next_offer(&wk, &ctx(T0 + HOUR_MS, 1, true, 0), &c);
    assert_eq!(o.cands[0].plan.kind, BlockKind::Recover);
    assert_eq!(o.cands[0].plan.spec.bpm, 90);
    assert_eq!(kinds(&o).last(), Some(&BlockKind::Free));
    // Recover has a floor.
    let wl = ledger(&[weak(T0, "pulse:quarter", q(20), false)]);
    let o = next_offer(&wl, &ctx(T0 + HOUR_MS, 1, true, 0), &c);
    assert_eq!(o.cands[0].plan.spec.bpm, 20);
    // Probe needs a Good block even after a long pause.
    assert_ne!(
        first_kind(&wk, ctx(T0 + 100 * HOUR_MS, 0, false, 0)),
        BlockKind::Probe
    );
    // Stretch comes before Free; rhythm candidate before the tempo one.
    let o = next_offer(&strong, &ctx(soon, 1, false, 0), &c);
    assert_eq!(
        kinds(&o),
        vec![BlockKind::Stretch, BlockKind::Stretch, BlockKind::Free]
    );
    assert_eq!(
        o.cands[0].plan.spec,
        BlockSpec {
            exercise: Exercise::OnClickEighths,
            ..c.starter
        }
    );
    assert_eq!(o.change, Some((Axis::Rhythm, 0, 1)));
    assert_eq!(o.cands[1].plan.spec, q(105));
    // Rhythm gate: below 80 bpm only the tempo candidate remains.
    let slow = ledger(&[blk(T0, "pulse:quarter", q(70), 40, 0, false)]);
    let o = next_offer(&slow, &ctx(soon, 1, false, 0), &c);
    assert_eq!(o.cands[0].plan.spec, q(74));
    assert_eq!(o.change, Some((Axis::Tempo, 70, 74)));
    // Minimum +2 bpm at low tempo: 30 * 1.05 rounds to 32 (also +2).
    let lo = ledger(&[blk(T0, "pulse:quarter", q(30), 40, 0, false)]);
    let o = next_offer(&lo, &ctx(soon, 1, false, 0), &c);
    assert_eq!(o.cands[0].plan.spec.bpm, 32);
}

// ---------- 4. probe ----------

#[test]
fn probe_rules() {
    let c = cfg();
    let long = BlockSpec { bars: 30, ..q(60) };
    let l = ledger(&[
        good(T0, "pulse:quarter", q(80)),
        good(T0 + 3 * HOUR_MS, "pulse:eighth-down", long),
    ]);
    let end = T0 + 3 * HOUR_MS + 30_000;
    assert_eq!(l.last_block_end_ms, Some(end));
    let gap = 48 * HOUR_MS;
    let none = next_offer(&l, &ctx(end + gap - 1, 0, false, 0), &c);
    assert_ne!(none.cands[0].plan.kind, BlockKind::Probe);
    let o47 = next_offer(&l, &ctx(end + 47 * HOUR_MS + 59 * 60_000, 0, false, 0), &c);
    assert_ne!(o47.cands[0].plan.kind, BlockKind::Probe);
    let o = next_offer(&l, &ctx(end + gap, 0, false, 0), &c);
    let p = &o.cands[0].plan;
    assert_eq!(p.kind, BlockKind::Probe);
    assert!(!p.show_summary);
    assert!(o.change.is_none());
    // Identical to the last Good block, bars cut to <= 60 s.
    assert_eq!((p.spec.exercise, p.spec.bpm), (long.exercise, long.bpm));
    assert_eq!(p.spec.bars, 15);
    assert!(f64::from(p.spec.bars) * 240.0 / f64::from(p.spec.bpm) <= 60.0);
    // A short block is not changed.
    let l2 = ledger(&[good(T0, "pulse:quarter", q(80))]);
    let o = next_offer(&l2, &ctx(T0 + 30_000 + gap, 0, false, 0), &c);
    assert_eq!(o.cands[0].plan.spec, q(80));
    // Duration bound for any spec.
    for bpm in 20..=240u16 {
        for bars in [1u8, 2, 8, 16, 32, 255] {
            let s = cut_to_probe(BlockSpec { bars, bpm, ..q(60) });
            assert!(s.bars >= 1 && s.bars <= bars);
            assert!(f64::from(s.bars) * 240.0 / f64::from(bpm) <= 60.0);
        }
    }
    // Not repeated in the session.
    let o = next_offer(&l, &ctx(end + gap, 0, true, 1), &c);
    assert!(!kinds(&o).contains(&BlockKind::Probe));
    // Skipping logs `ProbeSkipped` at no cost and moves to the next rule.
    let o = next_offer(&l, &ctx(end + gap, 0, false, 0), &c);
    assert_eq!(choice_for_else(&o), Choice::ProbeSkipped);
    let nxt = something_else(&o).unwrap();
    assert_eq!(nxt.cands[nxt.chosen].plan.kind, BlockKind::Familiar);
    assert_eq!(choice_for_else(&nxt), Choice::SomethingElse);
    let exact = something_else_in(&o, &l, &ctx(end + gap, 0, false, 0), &c).unwrap();
    assert_eq!(exact.chosen, nxt.chosen);
    // The list wraps: always something, and a skipped probe is not offered again.
    let mut cur = o.clone();
    let mut seen = Vec::new();
    for _ in 0..o.cands.len() + 2 {
        cur = something_else(&cur).unwrap();
        assert_ne!(cur.cands[cur.chosen].plan.kind, BlockKind::Probe);
        seen.push(cur.chosen);
    }
    assert!((1..o.cands.len()).all(|i| seen.contains(&i)));
    // `something_else_in` agrees with the plain version on plans and gives the exact change.
    let strong = ledger(&[blk(T0, "pulse:quarter", q(100), 40, 0, false)]);
    let cx = ctx(T0 + HOUR_MS, 1, false, 0);
    let o = next_offer(&strong, &cx, &c);
    let a = something_else(&o).unwrap();
    let b = something_else_in(&o, &strong, &cx, &c).unwrap();
    assert_eq!(a.chosen, b.chosen);
    assert_eq!(a.change, None);
    assert_eq!(b.change, Some((Axis::Tempo, 100, 105)));
    // A stale offer is refused.
    assert!(something_else_in(&o, &l, &cx, &c).is_none());
}

// ---------- 5. probe does not depend on the domain ----------

#[test]
fn probe_ignores_domain() {
    let c = cfg();
    let base = ledger(&[good(T0, "pulse:quarter", q(90))]);
    let mut hi = base.clone();
    let mut lo = base.clone();
    hi.skills.get_mut(&key("pulse:quarter")).unwrap().a = 60.0;
    lo.skills.get_mut(&key("pulse:quarter")).unwrap().b = 60.0;
    lo.skills.get_mut(&key("pulse:quarter")).unwrap().a = 0.0;
    assert_ne!(
        hi.mean(&key("pulse:quarter")),
        lo.mean(&key("pulse:quarter"))
    );
    let cx = ctx(T0 + 30_000 + 48 * HOUR_MS, 0, false, 3);
    let (a, b) = (next_offer(&hi, &cx, &c), next_offer(&lo, &cx, &c));
    assert_eq!(a.cands[0].plan.kind, BlockKind::Probe);
    assert_eq!(a.cands[0], b.cands[0]);
    assert_eq!(a.cands[0].p_hat, None);
}

// ---------- 6. Stretch gate ----------

#[test]
fn stretch_needs_evidence_and_last_good() {
    let c = cfg();
    let has_stretch = |l: &LearnerLedger, now: i64| {
        kinds(&next_offer(l, &ctx(now, 1, true, 0), &c)).contains(&BlockKind::Stretch)
    };
    let soon = T0 + HOUR_MS;
    // n_ef 7: the block is NoReading and no skill state exists at all.
    assert!(!has_stretch(
        &ledger(&[blk(T0, "pulse:quarter", q(100), 7, 0, false)]),
        soon
    ));
    // n_ef 8 all successes: p - sd = 10/12 - sd; not enough yet.
    assert!(!has_stretch(
        &ledger(&[blk(T0, "pulse:quarter", q(100), 8, 0, false)]),
        soon
    ));
    assert!(has_stretch(
        &ledger(&[blk(T0, "pulse:quarter", q(100), 40, 0, false)]),
        soon
    ));
    // Last block graded Fair: no stretch although the domain is high.
    let l = ledger(&[
        blk(T0, "pulse:quarter", q(100), 40, 0, false),
        fair(T0 + HOUR_MS, "pulse:quarter", q(100)),
    ]);
    assert!(!has_stretch(&l, T0 + 2 * HOUR_MS));
    // Weak last block: no stretch.
    let l = ledger(&[
        blk(T0, "pulse:quarter", q(100), 40, 0, false),
        weak(T0 + HOUR_MS, "pulse:quarter", q(100), false),
    ]);
    assert!(!has_stretch(&l, T0 + 2 * HOUR_MS));
    // Forgetting: after a long time n_ef decays below 8.
    let strong = ledger(&[blk(T0, "pulse:quarter", q(100), 40, 0, false)]);
    assert!(!has_stretch(&strong, T0 + 200 * DAY_MS));
    // Property: every Stretch has a skill with n_ef >= 8 and last grade Good behind it.
    let mut r = Rng(99);
    for n in 0..10_000u64 {
        let (l, end) = random_ledger(&mut r);
        let now = end + r.range(0, 100 * 24) * HOUR_MS;
        let o = next_offer(&l, &ctx(now, 2, true, n), &c);
        if kinds(&o).contains(&BlockKind::Stretch) {
            let ok = l.skills.iter().any(|(k, s)| {
                s.last_grade == Grade::Good && l.n_ef(k) >= 8.0 && l.n_ef_at(k, now, &c) >= 8.0
            });
            assert!(ok);
        }
    }
}

// ---------- 7. exploration ----------

#[test]
fn explore_probabilities() {
    let strong = ledger(&[blk(T0, "pulse:quarter", q(100), 40, 0, false)]);
    let now = T0 + HOUR_MS;
    let mut c = cfg();
    // epsilon 0: p = 1 on the chosen one, 0 elsewhere, every time.
    for n in 0..200 {
        let o = next_offer(&strong, &ctx(now, 0, false, n), &c);
        assert_eq!(o.chosen, 0);
        assert_eq!(o.cands[0].p, 1.0);
        assert!(o.cands[1..].iter().all(|x| x.p == 0.0));
    }
    // epsilon 0.2.
    c.explore = 0.2;
    let probe_ctx = ctx(T0 + 30_000 + 48 * HOUR_MS, 0, false, 0);
    let n_cands = next_offer(&strong, &ctx(now, 0, false, 0), &c).cands.len();
    assert!(n_cands >= 3);
    let mut count = vec![0u32; n_cands];
    let draws = 10_000u64;
    for n in 0..draws {
        let o = next_offer(&strong, &ctx(now, 0, false, n), &c);
        let sum: f64 = o.cands.iter().map(|x| x.p).sum();
        assert!((sum - 1.0).abs() < 1e-12);
        count[o.chosen] += 1;
        // Same seed, same offer.
        assert_eq!(o, next_offer(&strong, &ctx(now, 0, false, n), &c));
        // The probe is never drawn and keeps p = 1.
        let po = next_offer(
            &strong,
            &SessionCtx {
                offer_no: n,
                ..probe_ctx
            },
            &c,
        );
        assert_eq!(po.chosen, 0);
        assert_eq!(po.cands[0].plan.kind, BlockKind::Probe);
        assert_eq!(po.cands[0].p, 1.0);
        let psum: f64 = po.cands.iter().map(|x| x.p).sum();
        assert!((psum - 1.0).abs() < 1e-12);
    }
    let o = next_offer(&strong, &ctx(now, 0, false, 0), &c);
    for (i, k) in count.iter().enumerate() {
        let freq = f64::from(*k) / draws as f64;
        assert!(
            (freq - o.cands[i].p).abs() < 0.02,
            "cand {i}: {freq} vs {}",
            o.cands[i].p
        );
    }
    // The seed follows the context.
    let a = next_offer(&strong, &ctx(now, 0, false, 1), &c).seed;
    let b = next_offer(&strong, &ctx(now, 0, false, 2), &c).seed;
    assert_ne!(a, b);
}

// ---------- 8. nothing public lists due items ----------

#[test]
fn no_public_due_list() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for f in ["schedule.rs", "picker.rs", "probe.rs"] {
        let src = std::fs::read_to_string(dir.join(f)).unwrap();
        let mut rest = src.as_str();
        while let Some(i) = rest.find("pub fn ") {
            let sig = &rest[i..];
            let end = sig.find('{').unwrap();
            let sig = &sig[..end];
            let ret = sig.split("->").nth(1).unwrap_or("");
            for bad in ["Vec", "Iterator", "[", "Set", "Map", "Option<Vec"] {
                assert!(
                    !ret.contains(bad),
                    "{f}: public fn returns a collection: {sig}"
                );
            }
            assert!(!sig.to_lowercase().contains("due"), "{f}: {sig}");
            rest = &rest[i + 7..];
        }
        if f == "schedule.rs" {
            assert!(
                !src.contains("pub fn "),
                "schedule.rs must be pub(crate) only"
            );
        }
    }
    // Behaviour: with many skills due, an offer carries at most one Review candidate.
    let mut l = LearnerLedger::new(&cfg());
    l.apply(&good(T0, "pulse:quarter", q(60)), &cfg());
    l.apply(&good(T0 + HOUR_MS, "pulse:eighth-down", e(60)), &cfg());
    l.apply(&good(T0 + 2 * HOUR_MS, "pulse:eighth-up", e(70)), &cfg());
    let now = T0 + 10 * DAY_MS;
    for n in 0..50 {
        let o = next_offer(&l, &ctx(now, 2, true, n), &cfg());
        let rev = o
            .cands
            .iter()
            .filter(|c| c.plan.kind == BlockKind::Review)
            .count();
        assert_eq!(rev, 1);
        // Oldest due first: the quarter skill.
        assert_eq!(o.cands[0].plan.skill, key("pulse:quarter"));
    }
}

// ---------- review fixes ----------

fn multi(ts: i64, spec: BlockSpec, parts: &[(&str, u32, u32)], kind: BlockKind) -> BlockRecord {
    let mut b = blk(ts, parts[0].0, spec, 0, 0, false);
    b.kind = kind;
    let mut id = 0;
    for (sk, s, f) in parts {
        let one = blk(ts, sk, spec, *s, *f, false);
        for mut e in one.evidence {
            e.id = id;
            id += 1;
            b.evidence.push(e);
        }
    }
    b
}

#[test]
fn probe_never_swallows_later_rules() {
    let c = cfg();
    // The only Good block is the starter: probe spec == comfort == starter.
    let l = ledger(&[good(T0, "pulse:quarter", c.starter)]);
    let cx = ctx(T0 + 30_000 + 48 * HOUR_MS, 0, false, 0);
    let o = next_offer(&l, &cx, &c);
    assert_eq!(kinds(&o)[0], BlockKind::Probe);
    assert!(kinds(&o).contains(&BlockKind::Familiar), "{:?}", kinds(&o));
    let nxt = something_else(&o).unwrap();
    assert_eq!(nxt.cands[nxt.chosen].plan.kind, BlockKind::Familiar);
    // Skipping again wraps without returning to the probe.
    let again = something_else(&nxt).unwrap();
    assert_ne!(again.cands[again.chosen].plan.kind, BlockKind::Probe);
    // Skipping the warm-up never leads to a Stretch as the first block.
    let strong = ledger(&[blk(T0, "pulse:quarter", q(100), 40, 0, false)]);
    let o = next_offer(&strong, &ctx(T0 + 30_000 + 48 * HOUR_MS, 0, false, 0), &c);
    assert_eq!(kinds(&o)[..2], [BlockKind::Probe, BlockKind::Familiar]);
}

#[test]
fn recover_does_not_chain() {
    let c = cfg();
    let mut l = ledger(&[weak(T0, "pulse:quarter", q(100), false)]);
    let o = next_offer(&l, &ctx(T0 + HOUR_MS, 1, true, 0), &c);
    assert_eq!(o.cands[0].plan.kind, BlockKind::Recover);
    assert_eq!(o.cands[0].plan.spec.bpm, 90);
    assert_eq!(o.change, None);
    // A Weak block that was itself a Recover: no second step down.
    let mut w = weak(T0 + 2 * HOUR_MS, "pulse:quarter", q(90), false);
    w.kind = BlockKind::Recover;
    l.apply(&w, &c);
    for t in [3, 30] {
        let o = next_offer(&l, &ctx(T0 + t * HOUR_MS, 1, true, 0), &c);
        assert!(!kinds(&o).contains(&BlockKind::Recover), "{:?}", kinds(&o));
    }
    // The floor never repeats an identical Recover.
    let mut l = ledger(&[weak(T0, "pulse:quarter", q(20), false)]);
    let mut w = weak(T0 + HOUR_MS, "pulse:quarter", q(20), false);
    w.kind = BlockKind::Recover;
    l.apply(&w, &c);
    assert!(
        !kinds(&next_offer(&l, &ctx(T0 + 2 * HOUR_MS, 1, true, 0), &c))
            .contains(&BlockKind::Recover)
    );
    // After a failed tempo Stretch, go back to comfort, not -10%.
    let mut l = ledger(&[blk(T0, "pulse:quarter", q(100), 40, 0, false)]);
    let mut w = weak(T0 + HOUR_MS, "pulse:quarter", q(105), false);
    w.kind = BlockKind::Stretch;
    l.apply(&w, &c);
    let o = next_offer(&l, &ctx(T0 + 2 * HOUR_MS, 1, true, 0), &c);
    assert_eq!(o.cands[0].plan.kind, BlockKind::Recover);
    assert_eq!(o.cands[0].plan.spec.bpm, 100);
    // Free offers carry no change.
    let l = ledger(&[good(T0, "pulse:quarter", q(100))]);
    let o = next_offer(&l, &ctx(T0 + HOUR_MS, 1, true, 0), &c);
    let free = something_else_in(&o, &l, &ctx(T0 + HOUR_MS, 1, true, 0), &c).unwrap();
    assert_eq!(free.cands[free.chosen].plan.kind, BlockKind::Free);
    assert_eq!(free.change, None);
    // A weak free-play block does not trigger Recover.
    let l = ledger(&[weak(T0, "pulse:free", q(100), false)]);
    assert!(
        !kinds(&next_offer(&l, &ctx(T0 + HOUR_MS, 1, true, 0), &c)).contains(&BlockKind::Recover)
    );
}

#[test]
fn stretch_respects_sibling_skills() {
    let c = cfg();
    let eighths = |bs: &[BlockRecord]| ledger(bs);
    let any_stretch = |l: &LearnerLedger, t: i64| {
        let o = next_offer(l, &ctx(t, 1, true, 0), &c);
        kinds(&o).contains(&BlockKind::Stretch)
    };
    // Down Good, up Weak in the last block: no Stretch at all.
    let l = eighths(&[multi(
        T0,
        e(100),
        &[("pulse:eighth-down", 40, 0), ("pulse:eighth-up", 0, 10)],
        BlockKind::Familiar,
    )]);
    assert!(!any_stretch(&l, T0 + HOUR_MS));
    // Down Good, up Fair: no tempo Stretch for the eighths.
    let l = eighths(&[multi(
        T0,
        e(100),
        &[("pulse:eighth-down", 40, 0), ("pulse:eighth-up", 7, 3)],
        BlockKind::Familiar,
    )]);
    assert!(!any_stretch(&l, T0 + HOUR_MS));
    // Both Good: the tempo Stretch is there.
    let l = eighths(&[multi(
        T0,
        e(100),
        &[("pulse:eighth-down", 40, 0), ("pulse:eighth-up", 40, 0)],
        BlockKind::Familiar,
    )]);
    assert!(any_stretch(&l, T0 + HOUR_MS));
    // A new rhythm is not new when any skill of it already has comfort.
    let l = eighths(&[
        good(T0, "pulse:eighth-up", e(60)),
        multi(
            T0 + HOUR_MS,
            e(60),
            &[("pulse:eighth-down", 7, 3)],
            BlockKind::Familiar,
        ),
        blk(T0 + 2 * HOUR_MS, "pulse:quarter", q(100), 40, 0, false),
    ]);
    let o = next_offer(&l, &ctx(T0 + 3 * HOUR_MS, 1, true, 0), &c);
    assert!(kinds(&o).contains(&BlockKind::Stretch));
    assert!(
        o.cands
            .iter()
            .filter(|x| x.plan.kind == BlockKind::Stretch)
            .all(|x| x.plan.spec.exercise == Exercise::OnClickQuarters)
    );
}
