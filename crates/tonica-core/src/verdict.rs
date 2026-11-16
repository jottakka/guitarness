//! Verdict vocabulary shared by the judge, the session and the tape.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VerdictType {
    Attack,
    Timing,
    Tendency,
    Direction,
    Accent,
    Muted,
    ChordIdentity,
    StringMissing,
    StringExtra,
    WrongFret,
}

/// Why something was not judged. Never a failure of the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Why {
    NoCalibration,
    ClockNotLocked,
    Glitch,
    GateLocked,
    Disputed,
    Ghost,
    /// No attack was heard, so there is nothing to time. Counted under the attack verdict,
    /// never as a timing error or an abstention.
    NoAttack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attack {
    Heard,
    NotHeard,
    NotJudged(Why),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    Center,
    InWindow,
    Outside,
    NotJudged(Why),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    NotJudged(Why),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PulseVerdict {
    pub id: u32,
    pub attack: Attack,
    pub timing: Timing,
    /// Some only with a heard attack and a judged timing; negative = early.
    pub dt_ms: Option<f32>,
    pub dir: Direction,
    pub onset_in: Option<f64>,
    pub pos_in_bar: u8,
    pub fin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectKind {
    Attack,
    Ghost,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JudgeInput {
    Expect {
        id: u32,
        g_in: f64,
        kind: ExpectKind,
        prev_gap_s: f64,
        next_gap_s: f64,
        pos_in_bar: u8,
    },
    Heard {
        t_in: f64,
        judged: bool,
    },
    Strum {
        t_in: f64,
        judged: bool,
    },
    Unjudged {
        from_in: f64,
        to_in: f64,
    },
    Progress {
        in_now: f64,
        locked: bool,
    },
    Dispute,
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JudgeOut {
    Flash { id: Option<u32>, dt_ms: Option<f32> },
    Verdict(PulseVerdict),
    Withdrawn { id: u32, why: Why },
    Extra { t_in: f64 },
}

pub trait Judge {
    fn push(&mut self, input: &JudgeInput, out: &mut Vec<JudgeOut>);
}
