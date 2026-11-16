//! N4 tests (4) and (5): J1 end to end (late dispute equals erasure, same next offer) and J5
//! (offers are identical with any shadow).

use std::fs;
use std::path::PathBuf;

use guitarness_core::gates::GateState;
use guitarness_core::pulse::Exercise;
use guitarness_core::verdict::VerdictType;
use guitarness_learn::mastery::{DAY_MS, LearnerLedger};
use guitarness_learn::picker::next_offer;
use guitarness_learn::shadow::{NoShadow, Shadow, observe_block, shadow_line};
use guitarness_learn::store::Store;
use guitarness_learn::types::{
    BlockKind, BlockRecord, BlockSpec, Evidence, Grade, LearnConfig, LogLine, Offer, Outcome,
    Provenance, SessionCtx, SkillKey,
};

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("guitarness-n4-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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

fn blk(ts: i64, spec: BlockSpec, skill: &str, s: u32, f: u32) -> BlockRecord {
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
            dt_ms: Some(i as f32 - 3.0),
        });
    }
    BlockRecord {
        ts_ms: ts,
        dur_ms: 30_000,
        session: 1,
        kind: BlockKind::Familiar,
        spec,
        after_gap: false,
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

// (4) J1 end to end through a file: late dispute == fold without the event; same next offer.
#[test]
fn late_dispute_equals_erasure_and_same_next_offer() {
    let cfg = LearnConfig::default();
    let t = Tmp::new("dispute");
    // Day 0: exactly 8 successes (Good). Day 3: another block. Disputing one day-0 hit leaves
    // 7 weighted lines: NoReading, so the agenda step it earned disappears.
    let blocks = vec![
        blk(10 * DAY_MS, q(60), "pulse:quarter", 8, 0),
        blk(11 * DAY_MS, q(60), "pulse:quarter", 9, 1),
        blk(13 * DAY_MS, q(65), "pulse:quarter", 10, 0),
    ];
    let mut st = Store::open(&t.0).unwrap();
    for b in &blocks {
        st.append(&LogLine::Block(b.clone())).unwrap();
    }
    // The late dispute arrives after the later blocks were already applied.
    st.append(&LogLine::Dispute {
        block_ts: blocks[0].ts_ms,
        id: 3,
    })
    .unwrap();
    drop(st);

    let st = Store::open(&t.0).unwrap();
    let with = LearnerLedger::fold(st.lines(), &cfg);
    let mut erased = blocks.clone();
    erased[0].evidence.retain(|e| e.id != 3);
    let mut without = LearnerLedger::new(&cfg);
    for b in &erased {
        without.apply(b, &cfg);
    }
    assert_eq!(with, without, "J1: dispute == erasure, exactly");

    // It mattered: the same log without the dispute line folds differently.
    let undisputed: Vec<LogLine> = st
        .lines()
        .iter()
        .filter(|l| !matches!(l, LogLine::Dispute { .. }))
        .cloned()
        .collect();
    assert_ne!(LearnerLedger::fold(&undisputed, &cfg), with);

    for now in [13 * DAY_MS + 40_000, 14 * DAY_MS, 20 * DAY_MS, 40 * DAY_MS] {
        for (blocks_this_session, probe_done, n) in [(0, false, 0), (2, true, 1), (3, true, 7)] {
            let ctx = SessionCtx {
                now_ms: now,
                blocks_this_session,
                probe_done,
                offer_no: n,
            };
            assert_eq!(
                next_offer(&with, &ctx, &cfg),
                next_offer(&without, &ctx, &cfg),
                "same next offer at {now}"
            );
        }
    }
}

/// A shadow that predicts anything, differently every time it is asked.
struct Loud {
    seen: std::cell::Cell<u64>,
}
impl Shadow for Loud {
    fn name(&self) -> &'static str {
        "loud"
    }
    fn observe(&mut self, _k: &SkillKey, g: Grade, ts_ms: i64) {
        self.seen
            .set(self.seen.get() ^ (ts_ms as u64) ^ (g as u64 + 1));
    }
    fn predict(&self, _k: &SkillKey, ts_ms: i64) -> Option<(f32, Option<i64>)> {
        self.seen.set(
            self.seen
                .get()
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1),
        );
        Some((0.99, Some(ts_ms - 1_000_000_000)))
    }
}

/// Play `n` offers: each offer is logged, a shadow line is logged, the chosen block is
/// "played" with outcomes taken from the offer index, and the ledger is rebuilt from the log.
fn run(sh: &mut dyn Shadow, n: u64) -> (Vec<Offer>, Vec<LogLine>) {
    let cfg = LearnConfig::default();
    let mut st = Store::memory();
    let mut offers = Vec::new();
    let mut now = 100 * DAY_MS;
    let mut blocks_this_session = 0;
    for i in 0..n {
        let led = LearnerLedger::fold(st.lines(), &cfg);
        let ctx = SessionCtx {
            now_ms: now,
            blocks_this_session,
            probe_done: i > 0,
            offer_no: i,
        };
        let offer = next_offer(&led, &ctx, &cfg);
        st.append(&LogLine::Offer(offer.clone())).unwrap();
        let plan = offer.cands[offer.chosen].plan.clone();
        let (s, f) = if i % 4 == 3 { (3, 7) } else { (10, 0) };
        let mut b = blk(now + 1_000, plan.spec, &plan.skill.0, s, f);
        b.kind = plan.kind;
        st.append(&LogLine::Block(b.clone())).unwrap();
        let mut scratch = led.clone();
        let grades = scratch.apply(&b, &cfg);
        observe_block(sh, &grades, b.ts_ms);
        if let Some(l) = shadow_line(sh, &plan.skill, b.ts_ms) {
            st.append(&l).unwrap();
        }
        offers.push(offer);
        // Alternate within a day and across days so review agenda steps come due.
        now += if i % 3 == 2 { 2 * DAY_MS } else { 60_000 };
        blocks_this_session = if i % 3 == 2 {
            0
        } else {
            blocks_this_session + 1
        };
    }
    (offers, st.lines().to_vec())
}

// (5) J5: the offer sequence is identical with NoShadow and with a shadow that predicts anything.
#[test]
fn shadow_never_changes_offers() {
    let (a, la) = run(&mut NoShadow, 30);
    let (b, lb) = run(&mut Loud { seen: 0.into() }, 30);
    assert_eq!(a, b);
    assert!(
        a.iter().any(|o| o.cands.len() > 1),
        "sequence exercises real choices"
    );
    // The loud shadow did write shadow lines, the quiet one none: the logs differ only there.
    assert!(la.iter().all(|l| !matches!(l, LogLine::Shadow(_))));
    let shadows = lb
        .iter()
        .filter(|l| matches!(l, LogLine::Shadow(_)))
        .count();
    assert_eq!(shadows, 30);
    let rest: Vec<&LogLine> = lb
        .iter()
        .filter(|l| !matches!(l, LogLine::Shadow(_)))
        .collect();
    assert_eq!(la.iter().collect::<Vec<_>>(), rest);
    // Shadow lines are inert in the fold.
    let cfg = LearnConfig::default();
    assert_eq!(
        LearnerLedger::fold(&la, &cfg),
        LearnerLedger::fold(&lb, &cfg)
    );
}
