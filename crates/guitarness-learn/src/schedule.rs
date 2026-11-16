//! Read side of the hidden review agenda (the verifier/learner design plan §2.3).
//!
//! The agenda is *written* by `mastery::LearnerLedger::apply` (step, due, comfort, last grade).
//! This module only reads it. Everything here is `pub(crate)`: nothing public returns a list
//! of due items (the player never sees a queue).

use guitarness_core::pulse::{Exercise, MAX_BPM, MIN_BPM};

use crate::mastery::{HOUR_MS, LastBlock, LearnerLedger, NOT_DUE_MS};
use crate::types::{Axis, BlockSpec, Grade, LearnConfig, SkillKey};

/// Rhythm ladder, easiest first.
const LADDER: [Exercise; 2] = [Exercise::OnClickQuarters, Exercise::OnClickEighths];

pub(crate) fn exercise_index(ex: Exercise) -> u16 {
    LADDER.iter().position(|e| *e == ex).unwrap_or(0) as u16
}

pub(crate) fn next_exercise(ex: Exercise) -> Option<Exercise> {
    LADDER.get(exercise_index(ex) as usize + 1).copied()
}

/// Exercise a skill belongs to (`None` for free play and unknown keys).
pub(crate) fn exercise_of_skill(k: &SkillKey) -> Option<Exercise> {
    match k.0.as_str() {
        "pulse:quarter" => Some(Exercise::OnClickQuarters),
        "pulse:eighth-down" | "pulse:eighth-up" => Some(Exercise::OnClickEighths),
        _ => None,
    }
}

/// Skill that stands for an exercise when a plan needs one key (eighths: the down stroke).
pub(crate) fn skill_for_exercise(ex: Exercise) -> SkillKey {
    SkillKey(
        match ex {
            Exercise::OnClickQuarters => "pulse:quarter",
            Exercise::OnClickEighths => "pulse:eighth-down",
        }
        .to_string(),
    )
}

/// The block a skill is measured against for the one-axis rule: its `comfort`, or, the first
/// time, the starter block played with the skill's own exercise.
pub(crate) fn baseline(l: &LearnerLedger, k: &SkillKey, cfg: &LearnConfig) -> BlockSpec {
    if let Some(c) = l.skills.get(k).and_then(|s| s.comfort) {
        return c;
    }
    BlockSpec {
        exercise: exercise_of_skill(k).unwrap_or(cfg.starter.exercise),
        ..cfg.starter
    }
}

/// The one axis (if exactly one) in which `spec` differs from `base`.
/// Tempo: `(bpm_old, bpm_new)`. Rhythm: `(ladder index old, ladder index new)`. `bars` is not an axis.
pub(crate) fn axis_change(base: &BlockSpec, spec: &BlockSpec) -> Option<(Axis, u16, u16)> {
    let rhythm = base.exercise != spec.exercise;
    let tempo = base.bpm != spec.bpm;
    match (rhythm, tempo) {
        (true, false) => Some((
            Axis::Rhythm,
            exercise_index(base.exercise),
            exercise_index(spec.exercise),
        )),
        (false, true) => Some((Axis::Tempo, base.bpm, spec.bpm)),
        _ => None,
    }
}

/// Pause threshold in ms (`probe_gap_hours`).
pub(crate) fn pause_ms(cfg: &LearnConfig) -> i64 {
    (cfg.probe_gap_hours * HOUR_MS as f64).round() as i64
}

/// True when at least the pause has passed since the end of the last block.
pub(crate) fn pause_elapsed(l: &LearnerLedger, now_ms: i64, cfg: &LearnConfig) -> bool {
    l.last_block_end_ms
        .is_some_and(|end| now_ms.saturating_sub(end) >= pause_ms(cfg))
}

fn has_agenda_comfort(l: &LearnerLedger, k: &SkillKey) -> Option<BlockSpec> {
    if k.0 == "pulse:free" {
        return None;
    }
    l.skills.get(k).and_then(|s| s.comfort)
}

/// Skill with the highest decayed `p̂` among those with a comfort block, with that comfort.
/// Ties go to the smaller key.
pub(crate) fn most_confident(
    l: &LearnerLedger,
    now_ms: i64,
    cfg: &LearnConfig,
) -> Option<(SkillKey, BlockSpec, f64)> {
    let mut best: Option<(SkillKey, BlockSpec, f64)> = None;
    for k in l.skills.keys() {
        let Some(c) = has_agenda_comfort(l, k) else {
            continue;
        };
        let p = l.mean_at(k, now_ms, cfg);
        if best.as_ref().is_none_or(|(_, _, bp)| p > *bp) {
            best = Some((k.clone(), c, p));
        }
    }
    best
}

/// The single oldest-due skill (`due <= now`) with its comfort; ties go to the smaller key.
/// Returns one item on purpose: the picker never exposes the whole queue.
pub(crate) fn oldest_due(l: &LearnerLedger, now_ms: i64) -> Option<(SkillKey, BlockSpec)> {
    let mut best: Option<(i64, SkillKey, BlockSpec)> = None;
    for (k, st) in &l.skills {
        let Some(c) = has_agenda_comfort(l, k) else {
            continue;
        };
        if st.step.is_none() || st.due_ms == NOT_DUE_MS || st.due_ms > now_ms {
            continue;
        }
        if best.as_ref().is_none_or(|(d, _, _)| st.due_ms < *d) {
            best = Some((st.due_ms, k.clone(), c));
        }
    }
    best.map(|(_, k, c)| (k, c))
}

/// First skill graded `Weak` in the last block, with that block.
pub(crate) fn weak_in_last_block(l: &LearnerLedger) -> Option<(SkillKey, &LastBlock)> {
    let lb = l.last_block.as_ref()?;
    lb.grades
        .iter()
        .find(|(k, g)| *g == Grade::Weak && k.0 != "pulse:free")
        .map(|(k, _)| (k.clone(), lb))
}

/// `Recover` tempo: -10%, rounded, floor at `MIN_BPM`.
pub(crate) fn tempo_down(bpm: u16) -> u16 {
    ((f64::from(bpm) * 0.9).round()).max(MIN_BPM) as u16
}

/// `Stretch` tempo: `+tempo_step_pct`, at least +2 BPM, ceiling `MAX_BPM`. `None` at the ceiling.
pub(crate) fn tempo_up(bpm: u16, cfg: &LearnConfig) -> Option<u16> {
    let pct = (f64::from(bpm) * (1.0 + cfg.tempo_step_pct / 100.0)).round();
    let up = pct.max(f64::from(bpm) + 2.0).min(MAX_BPM) as u16;
    (up > bpm).then_some(up)
}

/// Stretch eligibility: `p̂ - sd >= good`, `n_ef >= min_n` (both decayed to `now`), last graded
/// block `Good`, and a comfort block. Ordered by `p̂ - sd` descending, then key.
pub(crate) fn stretch_eligible(
    l: &LearnerLedger,
    now_ms: i64,
    cfg: &LearnConfig,
) -> Vec<(SkillKey, BlockSpec)> {
    let mut v: Vec<(f64, SkillKey, BlockSpec)> = Vec::new();
    // No Stretch right after a Weak block of any skill.
    if weak_in_last_block(l).is_some() {
        return Vec::new();
    }
    for (k, st) in &l.skills {
        let Some(c) = has_agenda_comfort(l, k) else {
            continue;
        };
        if st.last_grade != Grade::Good {
            continue;
        }
        let (a, b) = l.decayed(k, now_ms, cfg);
        let n = a + b;
        if n < cfg.min_n {
            continue;
        }
        let n0 = l.prior.0 + l.prior.1 + n;
        let m = (l.prior.0 + a) / n0;
        let sd = (m * (1.0 - m) / (n0 + 1.0)).sqrt();
        if m - sd >= cfg.good {
            v.push((m - sd, k.clone(), c));
        }
    }
    v.sort_by(|x, y| y.0.total_cmp(&x.0).then_with(|| x.1.cmp(&y.1)));
    v.into_iter().map(|(_, k, c)| (k, c)).collect()
}

/// Every skill of an exercise (eighths: down and up strokes).
pub(crate) fn skills_of_exercise(ex: Exercise) -> Vec<SkillKey> {
    match ex {
        Exercise::OnClickQuarters => vec![SkillKey("pulse:quarter".to_string())],
        Exercise::OnClickEighths => vec![
            SkillKey("pulse:eighth-down".to_string()),
            SkillKey("pulse:eighth-up".to_string()),
        ],
    }
}

/// True when no skill of the exercise last graded `Weak` or `Fair` (a tempo Stretch needs it).
pub(crate) fn exercise_clear(l: &LearnerLedger, ex: Exercise) -> bool {
    skills_of_exercise(ex).iter().all(|k| {
        l.skills
            .get(k)
            .is_none_or(|s| !matches!(s.last_grade, Grade::Weak | Grade::Fair))
    })
}
