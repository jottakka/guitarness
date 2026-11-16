//! Display ledger (the verifier/learner design plan §2.1): lifetime numbers shown to the player.
//! Every number is a `Mono`: it can only go up. Loading a file is `merge` (field-wise maximum),
//! so an older or tampered file never lowers anything. Disputes touch nothing; weak grades never
//! remove a mark or milestone. `Probe` blocks add to the totals but never to marks or milestones.

use std::collections::{BTreeMap, BTreeSet};

use crate::types::{BlockKind, BlockRecord, Grade, SkillKey};
use serde_json::{Value, json};
use tonica_core::pulse::Exercise;

const DAY_MS: i64 = 86_400_000;
const FORMAT_V: u64 = 1;

/// A number that never falls. The field is private; the only mutators are `add` and `raise`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mono(u64);

impl Mono {
    pub fn get(self) -> u64 {
        self.0
    }

    /// Saturating increase.
    pub fn add(&mut self, n: u64) {
        self.0 = self.0.saturating_add(n);
    }

    /// Becomes the larger of the two values.
    pub fn raise(&mut self, v: u64) {
        self.0 = self.0.max(v);
    }
}

fn exercise_name(e: Exercise) -> &'static str {
    match e {
        Exercise::OnClickQuarters => "quarters",
        Exercise::OnClickEighths => "eighths",
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayLedger {
    /// Heard attacks, lifetime.
    pub strums: Mono,
    pub blocks: Mono,
    pub active_ms: Mono,
    /// Distinct days played (a count, never a streak).
    pub days: Mono,
    /// Highest tempo with a `Good` block, per exercise name.
    pub best_bpm: BTreeMap<String, Mono>,
    /// Most heard attacks in one block.
    pub best_block_strums: Mono,
    /// Milestones; entries are only ever added.
    pub firsts: BTreeSet<String>,
    seen_days: BTreeSet<i64>,
}

impl DisplayLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold in one finished block. `grades` are the per-skill grades of that block.
    /// Probe blocks add totals only (J3).
    pub fn apply(&mut self, b: &BlockRecord, grades: &[(SkillKey, Grade)]) {
        self.strums.add(u64::from(b.strums));
        self.blocks.add(1);
        self.active_ms.add(u64::from(b.dur_ms));
        self.seen_days.insert(b.ts_ms.div_euclid(DAY_MS));
        self.days.raise(self.seen_days.len() as u64);
        if b.kind == BlockKind::Probe {
            return;
        }
        self.best_block_strums.raise(u64::from(b.strums));
        self.firsts.insert("first_block".into());
        if b.strums > 0 {
            self.firsts.insert("first_strum".into());
        }
        if grades.iter().any(|(_, g)| *g == Grade::Good) {
            let name = exercise_name(b.spec.exercise);
            self.best_bpm
                .entry(name.to_string())
                .or_default()
                .raise(u64::from(b.spec.bpm));
            self.firsts.insert("first_good_block".into());
            self.firsts.insert(format!("first_good_{name}"));
            let band = u64::from(b.spec.bpm) / 20 * 20;
            self.firsts.insert(format!("good_{name}_{band}"));
        }
    }

    /// A dispute changes no field (the credit is the block and the time played).
    pub fn on_dispute(&mut self, _block_ts: i64, _id: u32) {}

    /// Field-wise maximum / union. Never lowers anything.
    pub fn merge(&mut self, other: &DisplayLedger) {
        self.strums.raise(other.strums.get());
        self.blocks.raise(other.blocks.get());
        self.active_ms.raise(other.active_ms.get());
        self.best_block_strums.raise(other.best_block_strums.get());
        for (k, v) in &other.best_bpm {
            self.best_bpm.entry(k.clone()).or_default().raise(v.get());
        }
        self.firsts.extend(other.firsts.iter().cloned());
        self.seen_days.extend(other.seen_days.iter().copied());
        self.days.raise(other.days.get());
        self.days.raise(self.seen_days.len() as u64);
    }

    pub fn to_json(&self) -> String {
        let bests: serde_json::Map<String, Value> = self
            .best_bpm
            .iter()
            .map(|(k, v)| (k.clone(), json!(v.get())))
            .collect();
        json!({
            "v": FORMAT_V,
            "strums": self.strums.get(),
            "blocks": self.blocks.get(),
            "active_ms": self.active_ms.get(),
            "days": self.days.get(),
            "best_block_strums": self.best_block_strums.get(),
            "best_bpm": bests,
            "firsts": self.firsts.iter().collect::<Vec<_>>(),
            "seen_days": self.seen_days.iter().collect::<Vec<_>>(),
        })
        .to_string()
    }

    /// Lenient: missing or ill-typed fields read as zero/empty. `None` only if not a JSON object.
    pub fn from_json(s: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(s).ok()?;
        let o = v.as_object()?;
        let num = |k: &str| o.get(k).and_then(Value::as_u64).unwrap_or(0);
        let mut l = DisplayLedger::new();
        l.strums.raise(num("strums"));
        l.blocks.raise(num("blocks"));
        l.active_ms.raise(num("active_ms"));
        l.days.raise(num("days"));
        l.best_block_strums.raise(num("best_block_strums"));
        if let Some(m) = o.get("best_bpm").and_then(Value::as_object) {
            for (k, x) in m {
                if let Some(n) = x.as_u64() {
                    l.best_bpm.entry(k.clone()).or_default().raise(n);
                }
            }
        }
        if let Some(a) = o.get("firsts").and_then(Value::as_array) {
            l.firsts
                .extend(a.iter().filter_map(|x| x.as_str().map(String::from)));
        }
        if let Some(a) = o.get("seen_days").and_then(Value::as_array) {
            l.seen_days.extend(a.iter().filter_map(Value::as_i64));
        }
        l.days.raise(l.seen_days.len() as u64);
        Some(l)
    }
}
