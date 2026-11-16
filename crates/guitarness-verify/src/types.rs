//! Plain data types of the chord verifier (the verifier/learner design plan §4).

use guitarness_core::verdict::VerdictType;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Must(u8),
    May,
    MustNot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HypKind {
    Expected,
    Missing { string: u8 },
    Extra { string: u8 },
    WrongFret { string: u8, fret: u8 },
    Neighbour { chord: String },
    AllMuted,
}

/// `sounding[i]` is the fret heard on string `i`; -1 means the string does not sound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hypothesis {
    pub kind: HypKind,
    pub sounding: [i8; 6],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfusionSet {
    pub chord: String,
    pub ask: [Ask; 6],
    pub expected: Hypothesis,
    pub alts: Vec<Hypothesis>,
    pub tuning: [i32; 6],
    pub capo: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Window<'a> {
    pub fs: f64,
    pub pre: &'a [f32],
    pub post: &'a [f32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Abstain {
    GateLocked,
    LowSnr,
    TooShort,
    Unobservable,
    Ambiguous,
    Conflict,
    OneWitness,
    NoMargin,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Opinion {
    Supports { margin: f32 },
    Names { alts: u32, margin: f32 },
    Abstains(Abstain),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Dsp,
    Learned,
}

pub trait Witness {
    fn family(&self) -> Family;
    fn name(&self) -> &'static str;
    fn opine(&mut self, w: &Window, set: &ConfusionSet) -> Opinion;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChordOutcome {
    Ok,
    Wrong(HypKind),
    NotJudged(Abstain),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChordVerdict {
    pub outcome: ChordOutcome,
    pub vtype: VerdictType,
    pub margin: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VerifyConfig {
    pub ok_margin: f32,
    pub veto_margin: f32,
    pub accuse_margin: Option<f32>,
}

/// Verdict type reported for a hypothesis. `AllMuted` maps to `StringMissing` (no string sounds).
pub fn vtype_of(k: &HypKind) -> VerdictType {
    match k {
        HypKind::Expected | HypKind::Neighbour { .. } => VerdictType::ChordIdentity,
        HypKind::Missing { .. } | HypKind::AllMuted => VerdictType::StringMissing,
        HypKind::Extra { .. } => VerdictType::StringExtra,
        HypKind::WrongFret { .. } => VerdictType::WrongFret,
    }
}
