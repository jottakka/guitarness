//! Plain data types of the learner model (the verifier/learner design plan §4). Functions live in the unit modules.

use tonica_core::gates::GateState;
use tonica_core::pulse::Exercise;
use tonica_core::verdict::{VerdictType, Why};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SkillKey(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZeroWhy {
    NotHeard,
    NotJudged(Why),
    Disputed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
    Zero(ZeroWhy),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub id: u32,
    pub skill: SkillKey,
    pub vtype: VerdictType,
    pub outcome: Outcome,
    pub gate: GateState,
    /// Recorded, never shown.
    pub dt_ms: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockSpec {
    pub exercise: Exercise,
    pub bpm: u16,
    pub bars: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Probe,
    Familiar,
    Recover,
    Review,
    Stretch,
    Free,
}

/// Difficulty axis; `Changes` comes later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Tempo,
    Rhythm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grade {
    Good,
    Fair,
    Weak,
    NoReading,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileId {
    Guest,
    Local(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub judge_v: String,
    pub basis: String,
    pub gate: GateState,
    pub tape: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockRecord {
    pub ts_ms: i64,
    pub dur_ms: u32,
    pub session: u32,
    pub kind: BlockKind,
    pub spec: BlockSpec,
    pub after_gap: bool,
    pub strums: u32,
    pub evidence: Vec<Evidence>,
    pub prov: Provenance,
}

/// Design-start constants; tests depend on properties, not on these values.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnConfig {
    /// Beta prior (a, b).
    pub prior: (f64, f64),
    pub tau_days: f64,
    /// Weight of a `Provisional` verdict (0.5; 0.0 is the registered alternative).
    pub provisional_weight: f64,
    pub good: f64,
    pub weak: f64,
    pub min_n: f64,
    pub starter: BlockSpec,
    /// Offer exploration epsilon (default 0).
    pub explore: f64,
    pub rhythm_gate_bpm: u16,
    /// Tempo step in percent (minimum +2 BPM applied by the picker).
    pub tempo_step_pct: f64,
    pub probe_gap_hours: f64,
}

impl Default for LearnConfig {
    fn default() -> Self {
        Self {
            prior: (2.0, 2.0),
            tau_days: 14.0,
            provisional_weight: 0.5,
            good: 0.80,
            weak: 0.60,
            min_n: 8.0,
            starter: BlockSpec {
                exercise: Exercise::OnClickQuarters,
                bpm: 60,
                bars: 4,
            },
            explore: 0.0,
            rhythm_gate_bpm: 80,
            tempo_step_pct: 5.0,
            probe_gap_hours: 48.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockPlan {
    pub spec: BlockSpec,
    pub kind: BlockKind,
    pub skill: SkillKey,
    pub show_summary: bool,
}

/// The rule that produced it is `plan.kind`.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub plan: BlockPlan,
    pub p: f64,
    pub p_hat: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub cands: Vec<Candidate>,
    pub chosen: usize,
    pub change: Option<(Axis, u16, u16)>,
    pub seed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionCtx {
    pub now_ms: i64,
    pub blocks_this_session: u32,
    pub probe_done: bool,
    pub offer_no: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Accepted,
    SomethingElse,
    ProbeSkipped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShadowLine {
    pub name: String,
    pub skill: SkillKey,
    pub p: f32,
    pub due_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LogLine {
    Block(BlockRecord),
    Dispute { block_ts: i64, id: u32 },
    Offer(Offer),
    Choice(Choice),
    Shadow(ShadowLine),
}
