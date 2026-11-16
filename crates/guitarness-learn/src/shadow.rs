//! Shadow predictor interface (the verifier/learner design plan §4, J5). A shadow watches grades and may
//! predict; nothing in the picker or the ledgers ever reads it, so offers cannot depend on it.
//! FSRS is not available offline: only the interface and `NoShadow` exist (plan §12 item 4).

use crate::types::{Grade, LogLine, ShadowLine, SkillKey};

pub trait Shadow {
    fn name(&self) -> &'static str;
    fn observe(&mut self, k: &SkillKey, g: Grade, ts_ms: i64);
    /// `(p, due_ms)` for `k` at `ts_ms`, or `None` when the shadow has no opinion.
    fn predict(&self, k: &SkillKey, ts_ms: i64) -> Option<(f32, Option<i64>)>;
}

/// The default shadow: observes nothing, predicts nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoShadow;

impl Shadow for NoShadow {
    fn name(&self) -> &'static str {
        "none"
    }
    fn observe(&mut self, _k: &SkillKey, _g: Grade, _ts_ms: i64) {}
    fn predict(&self, _k: &SkillKey, _ts_ms: i64) -> Option<(f32, Option<i64>)> {
        None
    }
}

/// Feed the grades of one block to the shadow.
pub fn observe_block(sh: &mut dyn Shadow, grades: &[(SkillKey, Grade)], ts_ms: i64) {
    for (k, g) in grades {
        sh.observe(k, *g, ts_ms);
    }
}

/// The `shadow` log line for `k`, or `None` when the shadow predicts nothing.
pub fn shadow_line(sh: &dyn Shadow, k: &SkillKey, ts_ms: i64) -> Option<LogLine> {
    sh.predict(k, ts_ms).map(|(p, due_ms)| {
        LogLine::Shadow(ShadowLine {
            name: sh.name().to_string(),
            skill: k.clone(),
            p,
            due_ms,
        })
    })
}
