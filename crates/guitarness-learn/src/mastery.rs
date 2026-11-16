//! Per-skill Beta mastery with forgetting, block grade and the hidden review agenda
//! (the verifier/learner design plan §2.1, §2.3 agenda). The state is a pure fold of the log.

use std::collections::BTreeMap;

use crate::evidence::weight;
use crate::types::{
    BlockKind, BlockRecord, BlockSpec, Grade, LearnConfig, LogLine, Outcome, SkillKey, ZeroWhy,
};

pub const DAY_MS: i64 = 86_400_000;
pub const HOUR_MS: i64 = 3_600_000;
/// Review steps in days.
pub const STEPS: [i64; 5] = [1, 2, 4, 8, 16];
pub const MAX_STEP: u8 = 4;
/// Minimum time since the last step-up for a `Good` block to count as "another day".
pub const OTHER_DAY_MS: i64 = 20 * HOUR_MS;
/// "Never" sentinels; far enough from `i64` limits that subtraction cannot overflow.
pub const NEVER_MS: i64 = i64::MIN / 2;
pub const NOT_DUE_MS: i64 = i64::MAX / 2;

#[derive(Clone, Debug, PartialEq)]
pub struct SkillState {
    pub a: f64,
    pub b: f64,
    pub last_ms: i64,
    /// Agenda step 0..=4; `None` until the first `Good` block.
    pub step: Option<u8>,
    /// `NOT_DUE_MS` when the skill has no agenda.
    pub due_ms: i64,
    pub last_up_ms: i64,
    /// Spec of the last `Good` block of this skill (internal; may go down).
    pub comfort: Option<BlockSpec>,
    /// Grade of the last block that had a reading (`NoReading` leaves it alone).
    pub last_grade: Grade,
}

impl SkillState {
    fn new() -> Self {
        Self {
            a: 0.0,
            b: 0.0,
            last_ms: NEVER_MS,
            step: None,
            due_ms: NOT_DUE_MS,
            last_up_ms: NEVER_MS,
            comfort: None,
            last_grade: Grade::NoReading,
        }
    }
}

/// What the picker needs to know about the most recent block.
#[derive(Clone, Debug, PartialEq)]
pub struct LastBlock {
    pub ts_ms: i64,
    pub kind: BlockKind,
    pub spec: BlockSpec,
    pub grades: Vec<(SkillKey, Grade)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LearnerLedger {
    pub prior: (f64, f64),
    pub skills: BTreeMap<SkillKey, SkillState>,
    /// End (`ts_ms + dur_ms`) of the latest block.
    pub last_block_end_ms: Option<i64>,
    /// Spec of the latest `Good` block of any skill.
    pub last_good: Option<BlockSpec>,
    pub last_block: Option<LastBlock>,
}

/// Decay factor over `dt_ms`; 1 when time does not advance.
fn decay(dt_ms: i64, tau_days: f64) -> f64 {
    if dt_ms <= 0 {
        1.0
    } else {
        (-(dt_ms as f64) / (tau_days * DAY_MS as f64)).exp()
    }
}

/// Block grade from weighted success/failure sums (§2.1).
pub fn grade_of(s: f64, f: f64, cfg: &LearnConfig) -> Grade {
    let n = s + f;
    if n < cfg.min_n {
        return Grade::NoReading;
    }
    let r = s / n;
    if r >= cfg.good {
        Grade::Good
    } else if r < cfg.weak {
        Grade::Weak
    } else {
        Grade::Fair
    }
}

fn counts_in_agenda(k: &SkillKey) -> bool {
    k.0 != "pulse:free"
}

impl LearnerLedger {
    pub fn new(cfg: &LearnConfig) -> Self {
        Self {
            prior: cfg.prior,
            skills: BTreeMap::new(),
            last_block_end_ms: None,
            last_good: None,
            last_block: None,
        }
    }

    /// Reference path. A `Dispute` line anywhere in the log turns that evidence into a
    /// zero-weight `Disputed` line, so the result equals the fold of the log without the event.
    pub fn fold(log: &[LogLine], cfg: &LearnConfig) -> Self {
        let disputes: Vec<(i64, u32)> = log
            .iter()
            .filter_map(|l| match l {
                LogLine::Dispute { block_ts, id } => Some((*block_ts, *id)),
                _ => None,
            })
            .collect();
        let mut led = Self::new(cfg);
        for l in log {
            if let LogLine::Block(b) = l {
                if disputes.iter().any(|(ts, _)| *ts == b.ts_ms) {
                    let mut b = b.clone();
                    for e in &mut b.evidence {
                        if disputes.contains(&(b.ts_ms, e.id)) {
                            e.outcome = Outcome::Zero(ZeroWhy::Disputed);
                        }
                    }
                    led.apply(&b, cfg);
                } else {
                    led.apply(b, cfg);
                }
            }
        }
        led
    }

    /// Fold one block in. Returns the grade of each skill that appeared in the block, in
    /// skill order.
    pub fn apply(&mut self, b: &BlockRecord, cfg: &LearnConfig) -> Vec<(SkillKey, Grade)> {
        let t = b.ts_ms;
        let mut sums: BTreeMap<SkillKey, (f64, f64)> = BTreeMap::new();
        for e in &b.evidence {
            let sf = sums.entry(e.skill.clone()).or_insert((0.0, 0.0));
            let w = weight(e, cfg);
            // Zero weight returns early: no decay, no `last`.
            if w == 0.0 {
                continue;
            }
            let st = self
                .skills
                .entry(e.skill.clone())
                .or_insert_with(SkillState::new);
            if st.last_ms != NEVER_MS {
                let d = decay(t - st.last_ms, cfg.tau_days);
                st.a *= d;
                st.b *= d;
            }
            st.last_ms = st.last_ms.max(t);
            match e.outcome {
                Outcome::Success => {
                    st.a += w;
                    sf.0 += w;
                }
                Outcome::Failure => {
                    st.b += w;
                    sf.1 += w;
                }
                Outcome::Zero(_) => {}
            }
        }
        let mut grades = Vec::with_capacity(sums.len());
        for (k, (s, f)) in sums {
            let g = grade_of(s, f, cfg);
            if g != Grade::NoReading {
                if let Some(st) = self.skills.get_mut(&k) {
                    st.last_grade = g;
                    if counts_in_agenda(&k) {
                        Self::move_agenda(st, g, b);
                    }
                }
                if g == Grade::Good {
                    self.last_good = Some(b.spec);
                }
            }
            grades.push((k, g));
        }
        self.last_block_end_ms = Some(
            self.last_block_end_ms
                .map_or(t + i64::from(b.dur_ms), |p| p.max(t + i64::from(b.dur_ms))),
        );
        self.last_block = Some(LastBlock {
            ts_ms: t,
            kind: b.kind,
            spec: b.spec,
            grades: grades.clone(),
        });
        grades
    }

    fn move_agenda(st: &mut SkillState, g: Grade, b: &BlockRecord) {
        let t = b.ts_ms;
        match g {
            Grade::Good => {
                st.comfort = Some(b.spec);
                if st.step.is_none() || t - st.last_up_ms >= OTHER_DAY_MS {
                    let step = st.step.map_or(0, |s| (s + 1).min(MAX_STEP));
                    st.step = Some(step);
                    st.due_ms = t + STEPS[step as usize] * DAY_MS;
                    st.last_up_ms = t;
                }
            }
            Grade::Weak if b.after_gap => {
                if let Some(s) = st.step {
                    let s = s.saturating_sub(1);
                    st.step = Some(s);
                    st.due_ms = t + STEPS[s as usize] * DAY_MS;
                }
            }
            _ => {}
        }
    }

    /// `(a, b)` decayed to `now_ms` (never earlier than the state's own instant).
    pub fn decayed(&self, k: &SkillKey, now_ms: i64, cfg: &LearnConfig) -> (f64, f64) {
        match self.skills.get(k) {
            None => (0.0, 0.0),
            Some(st) => {
                let d = decay(now_ms - st.last_ms, cfg.tau_days);
                (st.a * d, st.b * d)
            }
        }
    }

    fn mean_of(&self, a: f64, b: f64) -> f64 {
        (self.prior.0 + a) / (self.prior.0 + self.prior.1 + a + b)
    }

    /// Posterior mean at the last update (no extra decay).
    pub fn mean(&self, k: &SkillKey) -> f64 {
        let (a, b) = self.skills.get(k).map_or((0.0, 0.0), |s| (s.a, s.b));
        self.mean_of(a, b)
    }

    pub fn sd(&self, k: &SkillKey) -> f64 {
        let (a, b) = self.skills.get(k).map_or((0.0, 0.0), |s| (s.a, s.b));
        let m = self.mean_of(a, b);
        let n0 = self.prior.0 + self.prior.1 + a + b;
        (m * (1.0 - m) / (n0 + 1.0)).sqrt()
    }

    /// Effective evidence count `a + b`.
    pub fn n_ef(&self, k: &SkillKey) -> f64 {
        self.skills.get(k).map_or(0.0, |s| s.a + s.b)
    }

    /// Posterior mean after decaying to `now_ms`.
    pub fn mean_at(&self, k: &SkillKey, now_ms: i64, cfg: &LearnConfig) -> f64 {
        let (a, b) = self.decayed(k, now_ms, cfg);
        self.mean_of(a, b)
    }

    pub fn n_ef_at(&self, k: &SkillKey, now_ms: i64, cfg: &LearnConfig) -> f64 {
        let (a, b) = self.decayed(k, now_ms, cfg);
        a + b
    }
}
