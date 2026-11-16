//! Decision rule of the chord verifier (the verifier/learner design plan §2.4 table and §2.6 locks 3 and 4).
//!
//! `Names { alts, .. }` carries a bitmask over `set.alts`: bit `i` names `set.alts[i]`.
//! The expected hypothesis has no bit. Abstention is always available, and nothing is
//! accused unless two witnesses of different families agree on exactly one alternative,
//! a tape-derived `accuse_margin` exists and is met, and both the ChordIdentity gate and the
//! gate of the accused type are `Open`.

use guitarness_core::gates::{GateState, GateTable};
use guitarness_core::verdict::VerdictType;

use crate::types::{
    Abstain, ChordOutcome, ChordVerdict, ConfusionSet, Family, HypKind, Opinion, VerifyConfig,
    vtype_of,
};

fn not_judged(why: Abstain) -> ChordVerdict {
    ChordVerdict {
        outcome: ChordOutcome::NotJudged(why),
        vtype: VerdictType::ChordIdentity,
        margin: 0.0,
    }
}

/// Combine witness opinions into one verdict. Pure; never panics on malformed input.
pub fn decide(
    set: &ConfusionSet,
    ops: &[(Family, Opinion)],
    gates: &GateTable,
    cfg: &VerifyConfig,
) -> ChordVerdict {
    // Never judge while the chord gate is Locked (the builtin table, §2.6 lock 1); checked first.
    if gates.state(VerdictType::ChordIdentity) == GateState::Locked {
        return not_judged(Abstain::GateLocked);
    }
    // Unvalidated thresholds would silently disable the veto or the accusation margin.
    if !cfg.ok_margin.is_finite()
        || !cfg.veto_margin.is_finite()
        || cfg.accuse_margin.is_some_and(|a| !a.is_finite())
    {
        return not_judged(Abstain::Unavailable);
    }
    // A non-finite margin is a broken witness: treat the whole input as unavailable.
    let finite = |o: &Opinion| match *o {
        Opinion::Supports { margin } | Opinion::Names { margin, .. } => margin.is_finite(),
        Opinion::Abstains(_) => true,
    };
    if !ops.iter().all(|(_, o)| finite(o)) {
        return not_judged(Abstain::Unavailable);
    }

    let supports: Vec<f32> = ops
        .iter()
        .filter_map(|(_, o)| match *o {
            Opinion::Supports { margin } => Some(margin),
            _ => None,
        })
        .collect();
    let names: Vec<(Family, u32, f32)> = ops
        .iter()
        .filter_map(|(f, o)| match *o {
            Opinion::Names { alts, margin } => Some((*f, alts, margin)),
            _ => None,
        })
        .collect();
    let vetoed = names.iter().any(|&(_, _, m)| m >= cfg.veto_margin);

    // Ok: one strong Supports and no vetoing Names. `Provisional` or `Open` both allow it.
    if !vetoed {
        let best = supports.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if !supports.is_empty() && best >= cfg.ok_margin {
            return ChordVerdict {
                outcome: ChordOutcome::Ok,
                vtype: VerdictType::ChordIdentity,
                margin: best,
            };
        }
        if !supports.is_empty() || !names.is_empty() {
            return not_judged(Abstain::NoMargin);
        }
        let why = ops
            .iter()
            .find_map(|(_, o)| match *o {
                Opinion::Abstains(a) => Some(a),
                _ => None,
            })
            .unwrap_or(Abstain::Unavailable);
        return not_judged(why);
    }

    // Vetoed from here on. Any Supports contradicts it: conflict blocks both Ok and Wrong.
    if !supports.is_empty() {
        return not_judged(Abstain::Conflict);
    }
    // §2.6 lock 4: two witnesses of different families must both name something.
    let has = |fam: Family| names.iter().any(|&(f, _, _)| f == fam);
    if !(has(Family::Dsp) && has(Family::Learned)) {
        return not_judged(Abstain::OneWitness);
    }
    let mask = names.iter().fold(u32::MAX, |m, &(_, a, _)| m & a);
    match mask.count_ones() {
        0 => return not_judged(Abstain::Conflict),
        1 => {}
        _ => return not_judged(Abstain::Ambiguous),
    }
    let Some(alt) = set.alts.get(mask.trailing_zeros() as usize) else {
        return not_judged(Abstain::Conflict);
    };
    // §2.6 lock 4: no tape-derived margin, no accusation.
    let min_margin = names
        .iter()
        .map(|&(_, _, m)| m)
        .fold(f32::INFINITY, f32::min);
    match cfg.accuse_margin {
        Some(a) if min_margin >= a => {}
        _ => return not_judged(Abstain::NoMargin),
    }
    // §2.6 lock 3: only `Open` gates (chord and accused type) allow an accusation.
    if alt.kind == HypKind::Expected {
        return not_judged(Abstain::Conflict);
    }
    let vtype = vtype_of(&alt.kind);
    if gates.state(VerdictType::ChordIdentity) != GateState::Open
        || gates.state(vtype) != GateState::Open
    {
        return not_judged(Abstain::GateLocked);
    }
    ChordVerdict {
        outcome: ChordOutcome::Wrong(alt.kind.clone()),
        vtype,
        margin: min_margin,
    }
}
