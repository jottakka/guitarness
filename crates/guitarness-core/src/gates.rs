//! Gate table: which verdict types may be judged (owned by C3).
//!
//! A gate decides whether the judge may emit a verdict of a given type or must
//! answer `NotJudged(GateLocked)`. States:
//! - `Locked`: never judges (default of every type without evidence).
//! - `Provisional`: judges; evidence is synthetic only (virtual player in CI).
//! - `Open`: judges; backed by enough real tapes. Only real tapes open a gate.

use serde_json::{Map, Value, json};

use crate::verdict::VerdictType;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateState {
    Locked,
    Provisional,
    Open,
}

impl GateState {
    pub fn name(self) -> &'static str {
        match self {
            GateState::Locked => "locked",
            GateState::Provisional => "provisional",
            GateState::Open => "open",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "locked" => Some(GateState::Locked),
            "provisional" => Some(GateState::Provisional),
            "open" => Some(GateState::Open),
            _ => None,
        }
    }
}

/// Pass rule: at least `min_events` labeled events, `false_wrong / n` at most
/// `1 / max_false_wrong_per`, and (for `Open`) at least `min_real_tapes` real tapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateRule {
    pub min_events: u32,
    pub max_false_wrong_per: u32,
    pub min_real_tapes: u32,
}

impl Default for GateRule {
    fn default() -> Self {
        GateRule {
            min_events: 200,
            max_false_wrong_per: 200,
            min_real_tapes: 3,
        }
    }
}

/// Result of applying a rule to counted evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateCheck {
    Pass,
    Fail,
    TooFew,
}

impl GateCheck {
    pub fn name(self) -> &'static str {
        match self {
            GateCheck::Pass => "pass",
            GateCheck::Fail => "fail",
            GateCheck::TooFew => "too_few",
        }
    }
}

impl GateRule {
    /// Error-rate part of the rule, ignoring tape provenance.
    pub fn check(&self, events: u32, false_wrong: u32) -> GateCheck {
        if events < self.min_events || events == 0 {
            return GateCheck::TooFew;
        }
        // false_wrong / events <= 1 / max_false_wrong_per, in integers.
        if u64::from(false_wrong) * u64::from(self.max_false_wrong_per) <= u64::from(events) {
            GateCheck::Pass
        } else {
            GateCheck::Fail
        }
    }
}

/// Counted evidence behind a gate state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GateEvidence {
    pub events: u32,
    pub false_wrong: u32,
    pub synthetic_tapes: u32,
    pub real_tapes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateEntry {
    pub state: GateState,
    pub rule: GateRule,
    pub evidence: GateEvidence,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateError {
    Json(String),
    Format(String),
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::Json(m) => write!(f, "gates json: {m}"),
            GateError::Format(m) => write!(f, "gates format: {m}"),
        }
    }
}

impl std::error::Error for GateError {}

/// Every verdict type, in table order.
pub const ALL_TYPES: [VerdictType; 10] = [
    VerdictType::Attack,
    VerdictType::Timing,
    VerdictType::Tendency,
    VerdictType::Direction,
    VerdictType::Accent,
    VerdictType::Muted,
    VerdictType::ChordIdentity,
    VerdictType::StringMissing,
    VerdictType::StringExtra,
    VerdictType::WrongFret,
];

pub fn type_name(t: VerdictType) -> &'static str {
    match t {
        VerdictType::Attack => "attack",
        VerdictType::Timing => "timing",
        VerdictType::Tendency => "tendency",
        VerdictType::Direction => "direction",
        VerdictType::Accent => "accent",
        VerdictType::Muted => "muted",
        VerdictType::ChordIdentity => "chord_identity",
        VerdictType::StringMissing => "string_missing",
        VerdictType::StringExtra => "string_extra",
        VerdictType::WrongFret => "wrong_fret",
    }
}

pub fn type_from_name(s: &str) -> Option<VerdictType> {
    ALL_TYPES.iter().copied().find(|t| type_name(*t) == s)
}

/// True when synthetic evidence alone may earn this type a judging gate
/// (`Provisional`). Only the timing family qualifies: a virtual player can show
/// that onsets are detected, never that chord, string, fret, direction, accent or
/// mute accusations are right. Those types need real tapes to leave `Locked`.
pub fn synthetic_can_unlock(t: VerdictType) -> bool {
    matches!(
        t,
        VerdictType::Attack | VerdictType::Timing | VerdictType::Tendency
    )
}

const SYNTHETIC_LOCK_NOTE: &str = "synthetic evidence cannot unlock this type; needs real tapes";

/// Demote a `Provisional` that rests on no real tape, for a type synthetic
/// evidence cannot unlock. Returns the state to keep.
fn lock_synthetic(t: VerdictType, state: GateState, ev: &GateEvidence) -> GateState {
    if state == GateState::Provisional && ev.real_tapes == 0 && !synthetic_can_unlock(t) {
        GateState::Locked
    } else {
        state
    }
}

fn index(t: VerdictType) -> usize {
    ALL_TYPES.iter().position(|x| *x == t).unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateTable {
    entries: [GateEntry; 10],
}

impl GateTable {
    /// Compiled default (plan 2.7). `Attack` and `Timing` start `Provisional`.
    /// `Tendency` is not hard-locked: the judge decides it through
    /// `TimingRef::may_claim_tendency` (plan 2.1), so its row is `Provisional`
    /// and the gate never blocks it by itself. Every other type starts `Locked`.
    pub fn builtin() -> Self {
        let entries = ALL_TYPES.map(|t| {
            let (state, reason) = match t {
                VerdictType::Attack | VerdictType::Timing => (
                    GateState::Provisional,
                    "default: synthetic evidence only, no real tapes",
                ),
                VerdictType::Tendency => (
                    GateState::Provisional,
                    "default: decided by the timing reference (may_claim_tendency)",
                ),
                _ => (GateState::Locked, "default: no evidence"),
            };
            GateEntry {
                state,
                rule: GateRule::default(),
                evidence: GateEvidence::default(),
                reason: reason.to_string(),
            }
        });
        GateTable { entries }
    }

    pub fn entry(&self, t: VerdictType) -> &GateEntry {
        &self.entries[index(t)]
    }

    pub fn state(&self, t: VerdictType) -> GateState {
        self.entry(t).state
    }

    pub fn rule(&self, t: VerdictType) -> GateRule {
        self.entry(t).rule
    }

    /// True when the judge may emit a verdict of this type.
    pub fn judges(&self, t: VerdictType) -> bool {
        self.state(t) != GateState::Locked
    }

    pub fn set(&mut self, t: VerdictType, entry: GateEntry) {
        self.entries[index(t)] = entry;
    }

    /// State a gate earns from counted evidence: `Open` needs the rule and enough
    /// real tapes; synthetic-only evidence that clears the rate rule gives
    /// `Provisional`; anything else is `Locked`.
    pub fn earned_state(rule: &GateRule, ev: &GateEvidence) -> GateState {
        if rule.check(ev.events, ev.false_wrong) != GateCheck::Pass {
            return GateState::Locked;
        }
        if ev.real_tapes > 0 && ev.real_tapes >= rule.min_real_tapes {
            GateState::Open
        } else {
            GateState::Provisional
        }
    }

    /// Replace the evidence of a type and move its state to what the evidence earns.
    pub fn apply_evidence(&mut self, t: VerdictType, ev: GateEvidence, reason: &str) {
        let e = &mut self.entries[index(t)];
        let earned = Self::earned_state(&e.rule, &ev);
        e.state = lock_synthetic(t, earned, &ev);
        e.reason = if e.state != earned {
            format!("{SYNTHETIC_LOCK_NOTE}; {reason}")
        } else {
            reason.to_string()
        };
        e.evidence = ev;
    }

    pub fn to_json(&self) -> String {
        let mut gates = Map::new();
        for t in ALL_TYPES {
            let e = self.entry(t);
            gates.insert(
                type_name(t).to_string(),
                json!({
                    "state": e.state.name(),
                    "rule": {
                        "min_events": e.rule.min_events,
                        "max_false_wrong_per": e.rule.max_false_wrong_per,
                        "min_real_tapes": e.rule.min_real_tapes,
                    },
                    "evidence": {
                        "events": e.evidence.events,
                        "false_wrong": e.evidence.false_wrong,
                        "synthetic_tapes": e.evidence.synthetic_tapes,
                        "real_tapes": e.evidence.real_tapes,
                    },
                    "reason": e.reason,
                }),
            );
        }
        serde_json::to_string_pretty(&json!({ "version": 1, "gates": Value::Object(gates) }))
            .unwrap_or_default()
    }

    /// Parse a `gates.json`. Starts from `builtin()`; types absent from the file keep
    /// their default; unknown type names are ignored (they stay `Locked`, nothing
    /// is added). A malformed known entry is an error. An `Open` claim that its own
    /// evidence does not back is demoted to `Locked`.
    pub fn from_json(s: &str) -> Result<Self, GateError> {
        let v: Value = serde_json::from_str(s).map_err(|e| GateError::Json(e.to_string()))?;
        let version = v.get("version").and_then(Value::as_u64);
        if version != Some(1) {
            return Err(GateError::Format("version must be 1".into()));
        }
        let gates = v
            .get("gates")
            .and_then(Value::as_object)
            .ok_or_else(|| GateError::Format("missing gates object".into()))?;
        let mut table = GateTable::builtin();
        for (name, val) in gates {
            let Some(t) = type_from_name(name) else {
                continue;
            };
            let mut e = parse_entry(name, val)?;
            if e.state == GateState::Open
                && Self::earned_state(&e.rule, &e.evidence) != GateState::Open
            {
                e.state = GateState::Locked;
                e.reason = format!("open claim not backed by evidence; was: {}", e.reason);
            } else if lock_synthetic(t, e.state, &e.evidence) != e.state {
                e.state = GateState::Locked;
                e.reason = format!("{SYNTHETIC_LOCK_NOTE}; was: {}", e.reason);
            }
            table.set(t, e);
        }
        Ok(table)
    }
}

fn get_u32(obj: &Value, key: &str, ctx: &str) -> Result<u32, GateError> {
    obj.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| GateError::Format(format!("{ctx}.{key} must be a u32")))
}

fn parse_entry(name: &str, val: &Value) -> Result<GateEntry, GateError> {
    let state_s = val
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| GateError::Format(format!("{name}.state missing")))?;
    let state = GateState::parse(state_s)
        .ok_or_else(|| GateError::Format(format!("{name}.state unknown: {state_s}")))?;
    let rule_v = val
        .get("rule")
        .ok_or_else(|| GateError::Format(format!("{name}.rule missing")))?;
    let ctx = format!("{name}.rule");
    let rule = GateRule {
        min_events: get_u32(rule_v, "min_events", &ctx)?,
        max_false_wrong_per: get_u32(rule_v, "max_false_wrong_per", &ctx)?,
        min_real_tapes: get_u32(rule_v, "min_real_tapes", &ctx)?,
    };
    let ev_v = val
        .get("evidence")
        .ok_or_else(|| GateError::Format(format!("{name}.evidence missing")))?;
    let ctx = format!("{name}.evidence");
    let evidence = GateEvidence {
        events: get_u32(ev_v, "events", &ctx)?,
        false_wrong: get_u32(ev_v, "false_wrong", &ctx)?,
        synthetic_tapes: get_u32(ev_v, "synthetic_tapes", &ctx)?,
        real_tapes: get_u32(ev_v, "real_tapes", &ctx)?,
    };
    let reason = val
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(GateEntry {
        state,
        rule,
        evidence,
        reason,
    })
}
