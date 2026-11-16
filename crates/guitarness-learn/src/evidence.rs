//! Evidence of a round: final verdict per expected event, after withdrawals (the verifier/learner design plan §2.1, §2.2).

use std::collections::BTreeMap;

use tonica_core::chart::Chart;
use tonica_core::gates::{GateState, GateTable};
use tonica_core::pulse::expectations;
use tonica_core::verdict::{Attack, JudgeOut, PulseVerdict, Timing, VerdictType, Why};

use crate::types::{Evidence, LearnConfig, Outcome, SkillKey, ZeroWhy};

/// Skill key of the event at `pos_in_bar` (eighth slot) in `chart`.
pub fn skill_of(chart: &Chart, pos_in_bar: u8) -> SkillKey {
    let name = match chart.id.as_str() {
        "on-click-quarters" => "pulse:quarter",
        "on-click-eighths" if pos_in_bar.is_multiple_of(2) => "pulse:eighth-down",
        "on-click-eighths" => "pulse:eighth-up",
        _ => "pulse:free",
    };
    SkillKey(name.to_string())
}

fn zero_of(why: Why) -> Outcome {
    if why == Why::Disputed {
        Outcome::Zero(ZeroWhy::Disputed)
    } else {
        Outcome::Zero(ZeroWhy::NotJudged(why))
    }
}

/// Outcome and verdict type of a final verdict. `None` for a ghost (no line).
fn classify(v: &PulseVerdict) -> Option<(VerdictType, Outcome)> {
    match (v.attack, v.timing) {
        (Attack::NotJudged(Why::Ghost), _) => None,
        (Attack::NotJudged(w), _) => Some((VerdictType::Attack, zero_of(w))),
        (Attack::NotHeard, _) => Some((VerdictType::Attack, Outcome::Zero(ZeroWhy::NotHeard))),
        (Attack::Heard, Timing::Center | Timing::InWindow) => {
            Some((VerdictType::Timing, Outcome::Success))
        }
        (Attack::Heard, Timing::Outside) => Some((VerdictType::Timing, Outcome::Failure)),
        (Attack::Heard, Timing::NotJudged(w)) => Some((VerdictType::Timing, zero_of(w))),
    }
}

/// Evidence lines (ascending `id`) and the number of heard strums of one round.
///
/// The final verdict of an id is its last `fin` verdict; a later `Withdrawn` turns it into a
/// zero-weight line. Ghost events and `Extra` produce no line. A strum is a heard attack that
/// was not withdrawn. The gate recorded is the `Timing` gate at the time of the round.
pub fn evidence_of_round(
    chart: &Chart,
    outs: &[JudgeOut],
    gates: &GateTable,
) -> (Vec<Evidence>, u32) {
    enum Fin {
        Verdict(PulseVerdict),
        Withdrawn(Why),
    }
    let mut last: BTreeMap<u32, Fin> = BTreeMap::new();
    for o in outs {
        match *o {
            JudgeOut::Verdict(v) if v.fin => {
                last.insert(v.id, Fin::Verdict(v));
            }
            JudgeOut::Withdrawn { id, why } => {
                last.insert(id, Fin::Withdrawn(why));
            }
            _ => {}
        }
    }
    let exps = expectations(chart);
    let gate = gates.state(VerdictType::Timing);
    let mut ev = Vec::new();
    let mut strums = 0u32;
    for (id, fin) in last {
        let Some(exp) = exps.get(id as usize) else {
            continue;
        };
        if exp.ghost {
            continue;
        }
        let (vtype, outcome, dt_ms) = match fin {
            Fin::Verdict(v) => {
                let Some((t, o)) = classify(&v) else { continue };
                if v.attack == Attack::Heard {
                    strums += 1;
                }
                let dt = matches!(o, Outcome::Success | Outcome::Failure)
                    .then_some(v.dt_ms)
                    .flatten();
                (t, o, dt)
            }
            Fin::Withdrawn(Why::Ghost) => continue,
            Fin::Withdrawn(w) => (VerdictType::Attack, zero_of(w), None),
        };
        ev.push(Evidence {
            id,
            skill: skill_of(chart, exp.pos_in_bar),
            vtype,
            outcome,
            gate,
            dt_ms,
        });
    }
    (ev, strums)
}

/// Weight of one evidence line: zero for anything uncertain, by gate otherwise.
pub fn weight(e: &Evidence, cfg: &LearnConfig) -> f64 {
    match e.outcome {
        Outcome::Zero(_) => 0.0,
        Outcome::Success | Outcome::Failure => match e.gate {
            GateState::Open => 1.0,
            GateState::Provisional => cfg.provisional_weight,
            GateState::Locked => 0.0,
        },
    }
}
