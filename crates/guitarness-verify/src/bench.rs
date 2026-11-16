//! Synthetic bench of the chord verifier (the verifier/learner design plan, unit V4).
//!
//! The bench takes scenarios (a rendered strum, its closed confusion set and the truth about
//! what the hand did), asks a witness for its opinion, and then converts that opinion in the
//! **worst case**: the second witness, of the other family, always gives the same opinion. The
//! result goes through the real [`decide`] with every gate `Open` and a zero accusation floor,
//! so a false `Names` becomes a false accusation unless the mask is ambiguous. The output is a
//! [`ConfusionReport`] plus a per-confusion-pair table.
//!
//! Nothing here can unlock a gate: synthetic evidence never moves `ChordIdentity`,
//! `StringMissing`, `StringExtra` or `WrongFret` (plan K1). The bench only reports.
//!
//! Rendering lives in the test file (the synthesiser is a dev-dependency); a [`Scenario`] holds a
//! closure that renders its take on demand, so a long grid never sits in memory.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use guitarness_core::gates::{ALL_TYPES, GateEntry, GateEvidence, GateRule, GateState, GateTable};
use guitarness_core::report::{ConfusionReport, Observation, Outcome, TapeSource, Truth as Label};

use crate::decide::decide;
use crate::excite::{ExciteConfig, cut};
use crate::types::{
    Abstain, ChordOutcome, ChordVerdict, ConfusionSet, Family, HypKind, Opinion, VerifyConfig,
    Witness,
};

/// What the hand did, against the chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Truth {
    /// Played as written.
    Right,
    /// Played exactly `set.alts[i]`.
    Alt(usize),
    /// Played a mistake the closed set does not list (two wrong frets at once).
    OutOfSet,
}

/// Coarse class of a scenario, for the summary rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Right,
    Missing,
    WrongFret,
    Extra,
    Neighbour,
    Muted,
    OutOfSet,
}

impl Class {
    pub const ALL: [Class; 7] = [
        Class::Right,
        Class::Missing,
        Class::WrongFret,
        Class::Extra,
        Class::Neighbour,
        Class::Muted,
        Class::OutOfSet,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Class::Right => "right",
            Class::Missing => "missing",
            Class::WrongFret => "wrong_fret",
            Class::Extra => "extra",
            Class::Neighbour => "neighbour",
            Class::Muted => "muted",
            Class::OutOfSet => "out_of_set",
        }
    }

    pub fn of(k: &HypKind) -> Class {
        match k {
            HypKind::Expected => Class::Right,
            HypKind::Missing { .. } => Class::Missing,
            HypKind::WrongFret { .. } => Class::WrongFret,
            HypKind::Extra { .. } => Class::Extra,
            HypKind::Neighbour { .. } => Class::Neighbour,
            HypKind::AllMuted => Class::Muted,
        }
    }
}

/// Acoustic condition of a scenario.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cond {
    pub snr_db: f32,
    /// Reverb wet share (0 = dry).
    pub wet: f32,
    /// Laptop-microphone filter (false = flat microphone).
    pub laptop: bool,
    pub down: bool,
    /// First to last string, ms.
    pub spread_ms: f32,
}

impl Cond {
    /// "40 dB, dry": the clean condition of the sanity floor (any microphone, stroke, spread).
    pub fn clean(&self) -> bool {
        self.snr_db >= 40.0 && self.wet == 0.0
    }
}

/// Everything the tables need to know about a scenario.
#[derive(Clone, Debug)]
pub struct Tag {
    pub chord: String,
    pub class: Class,
    /// Truth label: "right", "Missing s2", "WrongFret s1 f3", "Neighbour C", "OutOfSet ...".
    pub label: String,
    /// For a neighbour: the neighbour's tonic differs from the chord's (Dm is not D's neighbour
    /// by tonic, C is).
    pub tonic_differs: bool,
    pub cond: Cond,
    pub seed: u32,
}

/// A rendered take and where the strum sits in it (samples).
pub struct Take {
    pub audio: Vec<f32>,
    pub fs: f64,
    pub attack: usize,
    pub last: usize,
    pub prev_end: usize,
}

type Render = Box<dyn Fn() -> Take + Send + Sync>;

pub struct Scenario {
    pub tag: Tag,
    pub set: Arc<ConfusionSet>,
    pub truth: Truth,
    pub render: Render,
}

/// One scenario after the witness and the worst-case second witness.
#[derive(Clone, Debug)]
pub struct Record {
    pub tag: Tag,
    pub truth: Truth,
    /// What the DSP witness said alone.
    pub dsp: Opinion,
    /// Labels of the hypotheses in a `Names` mask (empty otherwise).
    pub named: Vec<String>,
    /// The true hypothesis is in the `Names` mask.
    pub named_true: bool,
    /// After `decide`, with the second witness always agreeing and every gate `Open`.
    pub verdict: ChordVerdict,
    pub obs: Observation,
}

/// All records of a run, in grid order, with the time spent in the witness.
#[derive(Clone, Debug, Default)]
pub struct BenchReport {
    pub records: Vec<Record>,
    pub obs: Vec<Observation>,
    /// Total time inside `Witness::opine`, ns.
    pub opine_ns: u128,
}

/// Counts of the DSP witness's raw opinions over a set of records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub n: usize,
    pub supports: usize,
    /// `Names` that includes the true hypothesis.
    pub names_true: usize,
    /// `Names` that does not (a false name; for a correct strum any `Names`).
    pub names_other: usize,
    pub abstains: [usize; 9],
    /// After the worst-case second witness and `decide`.
    pub ok: usize,
    pub wrong_true: usize,
    pub wrong_false: usize,
}

const ABSTAINS: [Abstain; 9] = [
    Abstain::GateLocked,
    Abstain::LowSnr,
    Abstain::TooShort,
    Abstain::Unobservable,
    Abstain::Ambiguous,
    Abstain::Conflict,
    Abstain::OneWitness,
    Abstain::NoMargin,
    Abstain::Unavailable,
];

pub fn abstain_index(a: Abstain) -> usize {
    ABSTAINS.iter().position(|x| *x == a).unwrap_or(8)
}

pub fn abstain_name(a: Abstain) -> &'static str {
    match a {
        Abstain::GateLocked => "gate_locked",
        Abstain::LowSnr => "low_snr",
        Abstain::TooShort => "too_short",
        Abstain::Unobservable => "unobservable",
        Abstain::Ambiguous => "ambiguous",
        Abstain::Conflict => "conflict",
        Abstain::OneWitness => "one_witness",
        Abstain::NoMargin => "no_margin",
        Abstain::Unavailable => "unavailable",
    }
}

impl Tally {
    pub fn add(&mut self, r: &Record) {
        self.n += 1;
        match r.dsp {
            Opinion::Supports { .. } => self.supports += 1,
            Opinion::Names { .. } => {
                if r.named_true {
                    self.names_true += 1;
                } else {
                    self.names_other += 1;
                }
            }
            Opinion::Abstains(a) => self.abstains[abstain_index(a)] += 1,
        }
        match &r.verdict.outcome {
            ChordOutcome::Ok => self.ok += 1,
            ChordOutcome::Wrong(_) => {
                if r.obs.truth == Label::Wrong {
                    self.wrong_true += 1;
                } else {
                    self.wrong_false += 1;
                }
            }
            ChordOutcome::NotJudged(_) => {}
        }
    }

    pub fn names(&self) -> usize {
        self.names_true + self.names_other
    }

    pub fn abstained(&self) -> usize {
        self.abstains.iter().sum()
    }

    pub fn abstain(&self, a: Abstain) -> usize {
        self.abstains[abstain_index(a)]
    }
}

/// Share as a number in 0..=1, 0 for an empty denominator.
pub fn frac(k: usize, n: usize) -> f64 {
    if n == 0 { 0.0 } else { k as f64 / n as f64 }
}

/// Wilson 95% interval for `k` of `n` (same formula as the report's).
pub fn wilson(k: usize, n: usize) -> (f64, f64) {
    guitarness_core::report::wilson95(k as u32, n as u32)
}

/// Thresholds for the worst-case conversion, taken from the witness's own operating point: a
/// `Supports` that clears `support_margin` is an `Ok` (second witness agreeing), any `Names`
/// is a veto and an accusation floor of zero adds nothing on top of the witness's own margin.
pub fn worst_case_cfg(c: &ExciteConfig) -> VerifyConfig {
    VerifyConfig {
        ok_margin: c.support_margin,
        veto_margin: c.name_margin,
        accuse_margin: Some(0.0),
    }
}

/// Every type `Open`: the worst case for accusations. Only the bench builds this table, and
/// nothing derived from it is ever written as a gate.
pub fn open_gates() -> GateTable {
    let mut g = GateTable::builtin();
    for t in ALL_TYPES {
        g.set(
            t,
            GateEntry {
                state: GateState::Open,
                rule: GateRule::default(),
                evidence: GateEvidence::default(),
                reason: "bench: worst case, every gate open".to_string(),
            },
        );
    }
    g
}

/// Label of a hypothesis (strings are numbered from the lowest, `s0` = low E).
pub fn label_of(k: &HypKind) -> String {
    match k {
        HypKind::Expected => "right".to_string(),
        HypKind::Missing { string } => format!("Missing s{string}"),
        HypKind::Extra { string } => format!("Extra s{string}"),
        HypKind::WrongFret { string, fret } => format!("WrongFret s{string} f{fret}"),
        HypKind::Neighbour { chord } => format!("Neighbour {chord}"),
        HypKind::AllMuted => "AllMuted".to_string(),
    }
}

fn observe(truth: Truth, set: &ConfusionSet, v: &ChordVerdict, tape: u32) -> Observation {
    let hand_right = truth == Truth::Right;
    let by_hand = if hand_right {
        Label::Right
    } else {
        Label::Wrong
    };
    let (outcome, label) = match &v.outcome {
        ChordOutcome::Ok => (Outcome::Ok, by_hand),
        ChordOutcome::NotJudged(_) => (Outcome::NotJudged, by_hand),
        ChordOutcome::Wrong(kind) => {
            // An accusation is right only when it names what the hand did.
            let hit =
                matches!(truth, Truth::Alt(i) if set.alts.get(i).is_some_and(|h| h.kind == *kind));
            (
                Outcome::Wrong,
                if hit { Label::Wrong } else { Label::Right },
            )
        }
    };
    Observation {
        vtype: v.vtype,
        outcome,
        truth: label,
        borderline: false,
        source: TapeSource::Synthetic,
        tape,
    }
}

/// Run a grid: render, ask the witness, convert in the worst case, record.
pub fn run(grid: &[Scenario], w: &mut dyn Witness, cfg: &VerifyConfig) -> BenchReport {
    let gates = open_gates();
    let fam = w.family();
    let other = match fam {
        Family::Dsp => Family::Learned,
        Family::Learned => Family::Dsp,
    };
    let mut rep = BenchReport::default();
    for sc in grid {
        let take = (sc.render)();
        let win = cut(
            &take.audio,
            take.fs,
            take.attack,
            take.last,
            take.prev_end,
            take.audio.len(),
        );
        let t = Instant::now();
        let op = w.opine(&win, &sc.set);
        rep.opine_ns += t.elapsed().as_nanos();
        // Worst case: the second witness agrees with everything the first says.
        let verdict = decide(&sc.set, &[(fam, op), (other, op)], &gates, cfg);
        let mut named = Vec::new();
        let mut named_true = false;
        if let Opinion::Names { alts, .. } = op {
            for (i, h) in sc.set.alts.iter().enumerate().take(32) {
                if alts & (1u32 << i) != 0 {
                    named.push(label_of(&h.kind));
                }
            }
            named_true = matches!(sc.truth, Truth::Alt(t) if t < 32 && alts & (1u32 << t) != 0);
        }
        let obs = observe(sc.truth, &sc.set, &verdict, sc.tag.seed);
        rep.obs.push(obs);
        rep.records.push(Record {
            tag: sc.tag.clone(),
            truth: sc.truth,
            dsp: op,
            named,
            named_true,
            verdict,
            obs,
        });
    }
    rep
}

/// One row of the per-pair table: a truth label on one chord.
#[derive(Clone, Debug)]
pub struct PairRow {
    pub chord: String,
    pub label: String,
    pub class: Class,
    pub all: Tally,
    /// Only the 40 dB dry records.
    pub clean: Tally,
    /// What was named instead of the truth: label -> count.
    pub instead: BTreeMap<String, usize>,
}

impl BenchReport {
    pub fn merge(&mut self, o: BenchReport) {
        self.records.extend(o.records);
        self.obs.extend(o.obs);
        self.opine_ns += o.opine_ns;
    }

    pub fn tally(&self, pred: impl Fn(&Record) -> bool) -> Tally {
        let mut t = Tally::default();
        for r in self.records.iter().filter(|r| pred(r)) {
            t.add(r);
        }
        t
    }

    /// `ConfusionReport` over the records `pred` keeps, against the table `gates`.
    pub fn confusion(&self, gates: &GateTable, pred: impl Fn(&Record) -> bool) -> ConfusionReport {
        let obs: Vec<Observation> = self
            .records
            .iter()
            .filter(|r| pred(r))
            .map(|r| r.obs)
            .collect();
        ConfusionReport::build(&obs, gates)
    }

    /// Mean cost of one witness call, ms.
    pub fn mean_opine_ms(&self) -> f64 {
        if self.records.is_empty() {
            0.0
        } else {
            self.opine_ns as f64 / 1e6 / self.records.len() as f64
        }
    }

    /// Rows by (chord, truth label), in a stable order.
    pub fn pairs(&self) -> Vec<PairRow> {
        let mut rows: BTreeMap<(Class, String, String), PairRow> = BTreeMap::new();
        for r in &self.records {
            let key = (r.tag.class, r.tag.chord.clone(), r.tag.label.clone());
            let row = rows.entry(key).or_insert_with(|| PairRow {
                chord: r.tag.chord.clone(),
                label: r.tag.label.clone(),
                class: r.tag.class,
                all: Tally::default(),
                clean: Tally::default(),
                instead: BTreeMap::new(),
            });
            row.all.add(r);
            if r.tag.cond.clean() {
                row.clean.add(r);
            }
            if matches!(r.dsp, Opinion::Names { .. }) && !r.named_true {
                let what = if r.named.is_empty() {
                    "?".to_string()
                } else {
                    r.named.join(" + ")
                };
                *row.instead.entry(what).or_insert(0) += 1;
            }
        }
        rows.into_values().collect()
    }

    /// The whole report as Markdown: totals by class, one row per (chord, truth label), the
    /// names given instead of the truth, and the `ConfusionReport` text.
    pub fn table_md(&self, gates: &GateTable) -> String {
        let pct = |k: usize, n: usize| format!("{:.1}", 100.0 * frac(k, n));
        let mut s = String::new();
        let _ = writeln!(
            s,
            "Records: {}; witness cost {:.2} ms per event. Strings: s0 = low E. \
             Every figure is the DSP witness alone unless marked `decide`.\n",
            self.records.len(),
            self.mean_opine_ms()
        );

        let _ = writeln!(s, "## By class\n");
        let _ = writeln!(
            s,
            "| class | n | supports % | names(true) % | names(other) % | low_snr % | unobservable % | ambiguous % | other abst % | decide ok % | decide wrong(true) % | decide wrong(false) % |"
        );
        let _ = writeln!(s, "|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
        for c in Class::ALL {
            let t = self.tally(|r| r.tag.class == c);
            if t.n == 0 {
                continue;
            }
            let other = t.abstained()
                - t.abstain(Abstain::LowSnr)
                - t.abstain(Abstain::Unobservable)
                - t.abstain(Abstain::Ambiguous);
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                c.name(),
                t.n,
                pct(t.supports, t.n),
                pct(t.names_true, t.n),
                pct(t.names_other, t.n),
                pct(t.abstain(Abstain::LowSnr), t.n),
                pct(t.abstain(Abstain::Unobservable), t.n),
                pct(t.abstain(Abstain::Ambiguous), t.n),
                pct(other, t.n),
                pct(t.ok, t.n),
                pct(t.wrong_true, t.n),
                pct(t.wrong_false, t.n),
            );
        }

        let _ = writeln!(
            s,
            "\n## Per chord and truth (the confusion pairs)\n\n\
             `hint`: **named** = named correctly in >= 50% of the 40 dB dry events; \
             **approved** = `Supports` in >= 50% of all events (the witness accepts the mistake); \
             **abstains** otherwise. A hint is a reading aid, not a verdict.\n"
        );
        let _ = writeln!(
            s,
            "| chord | truth | n | supports % | named(true) % | named(other) % | low_snr | unobs | ambig | other | clean n | clean named(true) % | clean supports % | hint | named instead (top) |"
        );
        let _ = writeln!(
            s,
            "|---|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|---|---|"
        );
        for row in self.pairs() {
            let (a, c) = (&row.all, &row.clean);
            let other = a.abstained()
                - a.abstain(Abstain::LowSnr)
                - a.abstain(Abstain::Unobservable)
                - a.abstain(Abstain::Ambiguous);
            let hint = if row.class == Class::Right {
                "-"
            } else if frac(c.names_true, c.n) >= 0.5 {
                "named"
            } else if frac(a.supports, a.n) >= 0.5 {
                "approved"
            } else {
                "abstains"
            };
            let mut inst: Vec<(&String, &usize)> = row.instead.iter().collect();
            inst.sort_by(|x, y| y.1.cmp(x.1).then(x.0.cmp(y.0)));
            let inst = inst
                .iter()
                .take(2)
                .map(|(k, v)| format!("{k} x{v}"))
                .collect::<Vec<_>>()
                .join("; ");
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                row.chord,
                row.label,
                a.n,
                pct(a.supports, a.n),
                pct(a.names_true, a.n),
                pct(a.names_other, a.n),
                a.abstain(Abstain::LowSnr),
                a.abstain(Abstain::Unobservable),
                a.abstain(Abstain::Ambiguous),
                other,
                c.n,
                pct(c.names_true, c.n),
                pct(c.supports, c.n),
                hint,
                inst
            );
        }

        let _ = writeln!(
            s,
            "\n## Names given instead of the truth (truth -> named, all chords)\n"
        );
        let mut pairs: BTreeMap<(String, String), usize> = BTreeMap::new();
        for r in &self.records {
            if matches!(r.dsp, Opinion::Names { .. }) && !r.named_true {
                let truth = match r.truth {
                    Truth::Right => "right".to_string(),
                    _ => r.tag.label.split(' ').next().unwrap_or("?").to_string(),
                };
                let what = r
                    .named
                    .iter()
                    .map(|n| n.split(' ').next().unwrap_or("?").to_string())
                    .collect::<Vec<_>>()
                    .join("+");
                *pairs.entry((truth, what)).or_insert(0) += 1;
            }
        }
        let _ = writeln!(s, "| truth kind | named kind | count |");
        let _ = writeln!(s, "|---|---|--:|");
        for ((t, n), k) in &pairs {
            let _ = writeln!(s, "| {t} | {n} | {k} |");
        }

        let _ = writeln!(
            s,
            "\n## ConfusionReport, every record (worst case: second witness always agrees, all gates open)\n\n```"
        );
        s.push_str(&self.confusion(gates, |_| true).to_text());
        let _ = writeln!(s, "```\n");
        let _ = writeln!(
            s,
            "Gate state in this report is `Open` only because the bench opened it; \
             `proposed_gates` still locks the four chord types (K1)."
        );
        s
    }
}
