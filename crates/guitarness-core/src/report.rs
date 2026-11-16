//! Round and session report (owned by C3): per-verdict-type confusion report.
//!
//! Three error kinds are counted separately because they cost differently:
//! - `false_wrong`: the judge accused (wrong or not heard) and the player was right;
//! - `false_ok`: the judge accepted and the player was wrong;
//! - `abstain`: the judge declined (`NotJudged`) on a labeled event.
//!
//! Rates use the labeled, non-borderline, judged events as denominator `n`.
//! Abstentions are not in `n`: a judge that always abstains must not collect evidence.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::gates::{ALL_TYPES, GateCheck, GateEvidence, GateRule, GateState, GateTable, type_name};
use crate::verdict::VerdictType;

/// Where a tape came from. Only real tapes can open a gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TapeSource {
    Synthetic,
    Real,
}

/// What the judge said about one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Wrong,
    NotHeard,
    NotJudged,
    Disputed,
}

/// Ground truth about what the judge was asked: was the player right?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Truth {
    /// The player did it right: accusing is a false accusation.
    Right,
    /// The player did it wrong: accepting is a false acceptance.
    Wrong,
    /// No label: counted in the outcome totals, never in the rates.
    Unlabeled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub vtype: VerdictType,
    pub outcome: Outcome,
    pub truth: Truth,
    /// Within 5 ms of a window edge: counted, but outside the rates.
    pub borderline: bool,
    pub source: TapeSource,
    pub tape: u32,
}

/// Counts for one verdict type.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeReport {
    pub vtype: VerdictType,
    pub ok: u32,
    pub wrong: u32,
    pub not_heard: u32,
    pub not_judged: u32,
    pub disputed: u32,
    /// Labeled, non-borderline, judged events (the denominator).
    pub n: u32,
    pub false_wrong: u32,
    pub false_ok: u32,
    pub abstain: u32,
    pub borderline: u32,
    pub synthetic_tapes: u32,
    pub real_tapes: u32,
    pub false_wrong_rate: f64,
    pub false_ok_rate: f64,
    pub abstain_rate: f64,
    /// Wilson 95% upper bound on the false-wrong rate (1.0 when n = 0).
    pub false_wrong_upper: f64,
    pub false_ok_upper: f64,
    pub gate: GateCheck,
    /// State the evidence earns under the table's rule for this type.
    pub earned: GateState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConfusionReport {
    pub types: Vec<TypeReport>,
}

/// Wilson score interval at 95% for `k` successes in `n` trials. `n = 0` gives (0, 1).
pub fn wilson95(k: u32, n: u32) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    const Z: f64 = 1.959_963_984_540_054;
    let n = f64::from(n);
    let p = f64::from(k) / n;
    let z2 = Z * Z;
    let denom = 1.0 + z2 / n;
    let centre = p + z2 / (2.0 * n);
    let margin = Z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    (
        ((centre - margin) / denom).clamp(0.0, 1.0),
        ((centre + margin) / denom).clamp(0.0, 1.0),
    )
}

fn rate(k: u32, n: u32) -> f64 {
    if n == 0 {
        0.0
    } else {
        f64::from(k) / f64::from(n)
    }
}

impl ConfusionReport {
    /// Count observations against the rules of `gates`.
    pub fn build(obs: &[Observation], gates: &GateTable) -> Self {
        let types = ALL_TYPES
            .iter()
            .map(|&t| type_report(t, obs, gates.rule(t)))
            .collect();
        ConfusionReport { types }
    }

    pub fn get(&self, t: VerdictType) -> &TypeReport {
        self.types
            .iter()
            .find(|r| r.vtype == t)
            .unwrap_or(&self.types[0])
    }

    /// The table these observations justify: evidence replaced and states re-earned
    /// for every type that has at least one labeled event; other types are unchanged.
    pub fn proposed_gates(&self, base: &GateTable) -> GateTable {
        let mut out = base.clone();
        for r in &self.types {
            if r.n == 0 && r.abstain == 0 {
                continue;
            }
            let ev = GateEvidence {
                events: r.n,
                false_wrong: r.false_wrong,
                synthetic_tapes: r.synthetic_tapes,
                real_tapes: r.real_tapes,
            };
            let reason = format!("from {} labeled events", r.n);
            out.apply_evidence(r.vtype, ev, &reason);
        }
        out
    }

    pub fn to_json(&self) -> String {
        let types: Vec<Value> = self
            .types
            .iter()
            .map(|r| {
                json!({
                    "type": type_name(r.vtype),
                    "ok": r.ok, "wrong": r.wrong, "not_heard": r.not_heard,
                    "not_judged": r.not_judged, "disputed": r.disputed,
                    "n": r.n,
                    "false_wrong": r.false_wrong, "false_ok": r.false_ok,
                    "abstain": r.abstain, "borderline": r.borderline,
                    "synthetic_tapes": r.synthetic_tapes, "real_tapes": r.real_tapes,
                    "false_wrong_rate": r.false_wrong_rate,
                    "false_ok_rate": r.false_ok_rate,
                    "abstain_rate": r.abstain_rate,
                    "false_wrong_wilson95_upper": r.false_wrong_upper,
                    "false_ok_wilson95_upper": r.false_ok_upper,
                    "gate": r.gate.name(),
                    "earned_state": r.earned.name(),
                })
            })
            .collect();
        serde_json::to_string_pretty(&json!({ "version": 1, "types": types })).unwrap_or_default()
    }

    /// Plain-text table. Rates are point estimates; the Wilson column is the 95%
    /// upper bound, which is what the evidence actually supports.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        s.push_str(
            "type            ok wrong nheard njudged disp |    n  f_wrong  f_ok abstain bord | fw_rate fw_wilson95 | gate    earned\n",
        );
        for r in &self.types {
            s.push_str(&format!(
                "{:<14} {:>3} {:>5} {:>5} {:>7} {:>4} | {:>4} {:>8} {:>5} {:>7} {:>4} | {:>6.2}% {:>10.2}% | {:<7} {}\n",
                type_name(r.vtype),
                r.ok,
                r.wrong,
                r.not_heard,
                r.not_judged,
                r.disputed,
                r.n,
                r.false_wrong,
                r.false_ok,
                r.abstain,
                r.borderline,
                r.false_wrong_rate * 100.0,
                r.false_wrong_upper * 100.0,
                r.gate.name(),
                r.earned.name(),
            ));
        }
        s.push_str(
            "note: rates are point estimates; with few events the Wilson 95% upper bound is much higher.\n",
        );
        s
    }
}

fn type_report(t: VerdictType, all: &[Observation], rule: GateRule) -> TypeReport {
    let mut r = TypeReport {
        vtype: t,
        ok: 0,
        wrong: 0,
        not_heard: 0,
        not_judged: 0,
        disputed: 0,
        n: 0,
        false_wrong: 0,
        false_ok: 0,
        abstain: 0,
        borderline: 0,
        synthetic_tapes: 0,
        real_tapes: 0,
        false_wrong_rate: 0.0,
        false_ok_rate: 0.0,
        abstain_rate: 0.0,
        false_wrong_upper: 1.0,
        false_ok_upper: 1.0,
        gate: GateCheck::TooFew,
        earned: GateState::Locked,
    };
    let mut tapes: HashSet<(TapeSource, u32)> = HashSet::new();
    for o in all.iter().filter(|o| o.vtype == t) {
        match o.outcome {
            Outcome::Ok => r.ok += 1,
            Outcome::Wrong => r.wrong += 1,
            Outcome::NotHeard => r.not_heard += 1,
            Outcome::NotJudged => r.not_judged += 1,
            Outcome::Disputed => r.disputed += 1,
        }
        if o.borderline {
            r.borderline += 1;
            continue;
        }
        if o.truth == Truth::Unlabeled {
            continue;
        }
        match o.outcome {
            Outcome::Disputed => continue,
            Outcome::NotJudged => {
                r.abstain += 1;
                continue;
            }
            Outcome::Ok => {
                if o.truth == Truth::Wrong {
                    r.false_ok += 1;
                }
            }
            Outcome::Wrong | Outcome::NotHeard => {
                if o.truth == Truth::Right {
                    r.false_wrong += 1;
                }
            }
        }
        r.n += 1;
        tapes.insert((o.source, o.tape));
    }
    r.synthetic_tapes = tapes
        .iter()
        .filter(|(s, _)| *s == TapeSource::Synthetic)
        .count() as u32;
    r.real_tapes = tapes.len() as u32 - r.synthetic_tapes;
    r.false_wrong_rate = rate(r.false_wrong, r.n);
    r.false_ok_rate = rate(r.false_ok, r.n);
    r.abstain_rate = rate(r.abstain, r.n + r.abstain);
    r.false_wrong_upper = wilson95(r.false_wrong, r.n).1;
    r.false_ok_upper = wilson95(r.false_ok, r.n).1;
    r.gate = rule.check(r.n, r.false_wrong);
    r.earned = GateTable::earned_state(
        &rule,
        &GateEvidence {
            events: r.n,
            false_wrong: r.false_wrong,
            synthetic_tapes: r.synthetic_tapes,
            real_tapes: r.real_tapes,
        },
    );
    r
}
