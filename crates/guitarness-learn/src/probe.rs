//! Retention probe (the verifier/learner design plan §2.3 rule 1).
//!
//! The probe replays the last `Good` block of any skill, unadapted, cut to at most 60 s.
//! Whether it is offered depends only on the pause since the last block, on that last `Good`
//! spec and on the session flag: never on the mastery domain. Its result is never shown
//! (`show_summary == false`); it feeds the learner ledger and the display totals only.

use crate::mastery::LearnerLedger;
use crate::schedule::{pause_elapsed, skill_for_exercise};
use crate::types::{BlockKind, BlockPlan, BlockSpec, Choice, LearnConfig, Offer, SessionCtx};

/// Longest probe, in seconds.
pub const PROBE_MAX_SECS: u32 = 60;

/// `spec` with `bars` cut so that the block lasts at most `PROBE_MAX_SECS` (4/4, so
/// `bars * 240 / bpm` seconds), but never below one bar.
pub fn cut_to_probe(spec: BlockSpec) -> BlockSpec {
    let max_bars = (PROBE_MAX_SECS * u32::from(spec.bpm) / 240).max(1);
    BlockSpec {
        bars: u32::from(spec.bars).min(max_bars).max(1) as u8,
        ..spec
    }
}

/// The probe plan, if one is due: pause reached, a `Good` block exists, probe not yet done
/// or skipped this session.
pub(crate) fn probe_plan(
    l: &LearnerLedger,
    ctx: &SessionCtx,
    cfg: &LearnConfig,
) -> Option<BlockPlan> {
    if ctx.probe_done || !pause_elapsed(l, ctx.now_ms, cfg) {
        return None;
    }
    let good = l.last_good?;
    Some(BlockPlan {
        spec: cut_to_probe(good),
        kind: BlockKind::Probe,
        skill: skill_for_exercise(good.exercise),
        show_summary: false,
    })
}

/// What to log when the player asks for something else: skipping a probe is `ProbeSkipped`
/// (free of cost), anything else is `SomethingElse`.
pub fn choice_for_else(prev: &Offer) -> Choice {
    match prev.cands.get(prev.chosen) {
        Some(c) if c.plan.kind == BlockKind::Probe => Choice::ProbeSkipped,
        _ => Choice::SomethingElse,
    }
}
