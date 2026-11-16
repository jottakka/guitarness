//! Offer selector `rules-v1` (the verifier/learner design plan §2.3). One offer at a time; at most one axis
//! differs from the skill's own baseline (`comfort`, or the starter block the first time).
//! The candidate list is the rule order; the first candidate is the offer unless `explore > 0`.

use crate::mastery::LearnerLedger;
use crate::probe::probe_plan;
use crate::schedule::{
    axis_change, baseline, exercise_clear, exercise_index, most_confident, next_exercise,
    oldest_due, skill_for_exercise, skills_of_exercise, stretch_eligible, tempo_down, tempo_up,
    weak_in_last_block,
};
use crate::types::{
    Axis, BlockKind, BlockPlan, BlockSpec, Candidate, LearnConfig, Offer, SessionCtx, SkillKey,
};

type Change = Option<(Axis, u16, u16)>;

/// SplitMix64 (own copy; no dependencies).
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Seed of an offer: a pure function of the context, so the same context gives the same offer.
pub fn seed_of(ctx: &SessionCtx) -> u64 {
    let mut r = SplitMix64((ctx.now_ms as u64) ^ ctx.offer_no.rotate_left(32));
    r.next()
}

struct Built {
    cand: Candidate,
    change: Change,
}

fn plan(spec: BlockSpec, kind: BlockKind, skill: SkillKey) -> BlockPlan {
    BlockPlan {
        spec,
        kind,
        skill,
        show_summary: true,
    }
}

/// Candidates in rule order, `p` still 0. Probe first, `Free` always last (unless it repeats
/// an earlier spec, which is dropped).
fn build(l: &LearnerLedger, ctx: &SessionCtx, cfg: &LearnConfig) -> Vec<Built> {
    let now = ctx.now_ms;
    let mut v: Vec<Built> = Vec::new();
    let mut push = |p: BlockPlan, p_hat: Option<f64>, change: Change| {
        // The probe is never a duplicate source: only non-probe candidates dedupe each other.
        let dup = p.kind != BlockKind::Probe
            && v.iter()
                .any(|b| b.cand.plan.kind != BlockKind::Probe && b.cand.plan.spec == p.spec);
        if dup {
            return;
        }
        v.push(Built {
            cand: Candidate {
                plan: p,
                p: 0.0,
                p_hat,
            },
            change,
        });
    };

    // 1. Probe: no p_hat, no change (a warm-up replay, independent of the domain).
    if let Some(p) = probe_plan(l, ctx, cfg) {
        push(p, None, None);
    }

    // 2. Familiar: first real block of the session.
    let first_real = ctx.blocks_this_session == 0
        || (ctx.blocks_this_session == 1
            && l.last_block
                .as_ref()
                .is_some_and(|b| b.kind == BlockKind::Probe));
    if first_real {
        match most_confident(l, now, cfg) {
            Some((k, c, p)) => push(plan(c, BlockKind::Familiar, k), Some(p), None),
            None => {
                let k = skill_for_exercise(cfg.starter.exercise);
                let p = l.mean_at(&k, now, cfg);
                push(plan(cfg.starter, BlockKind::Familiar, k), Some(p), None);
            }
        }
    }

    // 3. Recover: same skill as the weak block. Never twice in a row (the chain stops and the
    // list falls through to Review/Free). After a failed Stretch go back to `comfort`; otherwise
    // tempo -10%. A Recover offer never reports a `change` (nothing downward is shown).
    if let Some((k, lb)) = weak_in_last_block(l).filter(|(_, lb)| lb.kind != BlockKind::Recover) {
        let base = baseline(l, &k, cfg);
        let bpm = if lb.kind == BlockKind::Stretch && base.bpm != lb.spec.bpm {
            base.bpm
        } else {
            tempo_down(lb.spec.bpm)
        };
        let spec = BlockSpec {
            exercise: base.exercise,
            bpm,
            bars: lb.spec.bars,
        };
        push(plan(spec, BlockKind::Recover, k), None, None);
    }

    // 4. Review: the single oldest due skill, at its comfort.
    if let Some((k, c)) = oldest_due(l, now) {
        let p = l.mean_at(&k, now, cfg);
        push(plan(c, BlockKind::Review, k), Some(p), None);
    }

    // 5. Stretch: at most two candidates, one axis each. New rhythm first, then tempo.
    let eligible = stretch_eligible(l, now, cfg);
    let rhythm = eligible.iter().find_map(|(_, c)| {
        let next = next_exercise(c.exercise)?;
        let target = skill_for_exercise(next);
        let fresh = skills_of_exercise(next)
            .iter()
            .all(|k| l.skills.get(k).is_none_or(|s| s.comfort.is_none()));
        (fresh && c.bpm >= cfg.rhythm_gate_bpm).then(|| {
            let spec = BlockSpec {
                exercise: next,
                ..cfg.starter
            };
            let change = Some((
                Axis::Rhythm,
                exercise_index(c.exercise),
                exercise_index(next),
            ));
            (target, spec, change)
        })
    });
    if let Some((k, spec, change)) = rhythm {
        push(plan(spec, BlockKind::Stretch, k), None, change);
    }
    let tempo = eligible.iter().find_map(|(k, c)| {
        if !exercise_clear(l, c.exercise) {
            return None;
        }
        tempo_up(c.bpm, cfg).map(|bpm| (k.clone(), *c, BlockSpec { bpm, ..*c }))
    });
    if let Some((k, c, spec)) = tempo {
        push(
            plan(spec, BlockKind::Stretch, k),
            None,
            axis_change(&c, &spec),
        );
    }

    // 6. Free: the starter block (in the baseline's exercise, so it stays within one axis).
    let k = skill_for_exercise(cfg.starter.exercise);
    let base = baseline(l, &k, cfg);
    let spec = BlockSpec {
        exercise: base.exercise,
        ..cfg.starter
    };
    push(plan(spec, BlockKind::Free, k), None, None);
    v
}

/// Assign `p` and choose. A leading probe is never drawn (`p = 1`, the rest 0). Otherwise
/// epsilon-greedy over the approved candidates: top gets `1 - e + e/n`, the others `e/n`.
fn choose(v: &mut [Built], cfg: &LearnConfig, seed: u64) -> usize {
    let n = v.len();
    let eps = if cfg.explore.is_finite() {
        cfg.explore.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let probe_first = v[0].cand.plan.kind == BlockKind::Probe;
    if probe_first || n == 1 || eps == 0.0 {
        for (i, b) in v.iter_mut().enumerate() {
            b.cand.p = if i == 0 { 1.0 } else { 0.0 };
        }
        return 0;
    }
    let share = eps / n as f64;
    for (i, b) in v.iter_mut().enumerate() {
        b.cand.p = if i == 0 { 1.0 - eps + share } else { share };
    }
    let mut rng = SplitMix64(seed);
    if rng.unit() < eps {
        (rng.next() % n as u64) as usize
    } else {
        0
    }
}

pub fn next_offer(l: &LearnerLedger, ctx: &SessionCtx, cfg: &LearnConfig) -> Offer {
    let mut v = build(l, ctx, cfg);
    let seed = seed_of(ctx);
    let chosen = choose(&mut v, cfg, seed);
    let change = v[chosen].change;
    Offer {
        cands: v.into_iter().map(|b| b.cand).collect(),
        chosen,
        change,
        seed,
    }
}

/// Index offered after `chosen`: the next candidate, wrapping to the start so the player always
/// gets something. A probe at the start is skipped on the wrap (it was already offered).
fn next_index(cands: &[Candidate], chosen: usize) -> usize {
    let n = cands.len();
    let i = (chosen + 1) % n;
    if i == 0 && n > 1 && cands[0].plan.kind == BlockKind::Probe {
        1
    } else {
        i
    }
}

/// The next candidate of the same list (wrapping, probe excluded on the wrap). Without the
/// ledger the one-axis `change` is unknown (`None`); use `something_else_in` to get it.
/// `None` only for an offer with no candidates.
pub fn something_else(prev: &Offer) -> Option<Offer> {
    if prev.cands.is_empty() {
        return None;
    }
    Some(Offer {
        cands: prev.cands.clone(),
        chosen: next_index(&prev.cands, prev.chosen),
        change: None,
        seed: prev.seed,
    })
}

/// Like `something_else`, with the exact `change` (the list is rebuilt from the same inputs).
/// `None` when `prev` no longer matches the ledger and context.
pub fn something_else_in(
    prev: &Offer,
    l: &LearnerLedger,
    ctx: &SessionCtx,
    cfg: &LearnConfig,
) -> Option<Offer> {
    let v = build(l, ctx, cfg);
    let same = v.len() == prev.cands.len()
        && v.iter()
            .zip(&prev.cands)
            .all(|(b, c)| b.cand.plan == c.plan);
    if !same || v.is_empty() {
        return None;
    }
    let chosen = next_index(&prev.cands, prev.chosen);
    Some(Offer {
        cands: prev.cands.clone(),
        chosen,
        change: v[chosen].change,
        seed: prev.seed,
    })
}
