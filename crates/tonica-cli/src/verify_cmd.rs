//! `tonica verify`: what the chord witness hears in recorded tapes (Guitarness plan V5).
//!
//! For every `Strum` event of a tape's chart that the judge matched to a heard onset, the
//! command cuts the `pre`/`post` windows out of the recorded microphone, asks the DSP witness
//! and prints one row per (take, chord). It only reports: it writes no gate table and
//! changes no verdict.
//!
//! Rules it keeps (V4 reading, plan section 2.6):
//! - While the chord gate is `Locked` nothing is presented as judged: every event is a
//!   "guia" (guide). The witness's reading is a measurement, not a verdict.
//! - With one witness there is never an accusation (`decide` needs two families). A DSP-only
//!   `Ok` counts for at most half a unit and only at chord level, never per string.
//! - A detuned guitar (`Fault::Detuned`) is neither right nor wrong; after three such strums
//!   in one tape the text suggests `tonica tune`.
//! - A strum whose tuning fit is not clear (`tune_ok == false`) gives no `Names` evidence and
//!   no hint.
//! - A string/kind hint is a measurement: only when the same `WrongFret` or `Extra` is the
//!   single name on two or more consecutive strums of one chord. `Neighbour` is never shown.
//!
//! The default text is short, in Brazilian Portuguese and has no numbers (chord symbols
//! excepted); `--detalhes` adds the table. Exit codes: 0 done, 20 a tape or file that
//! cannot be read, 2 bad command line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use guitarness_verify::bench::{abstain_name, label_of, worst_case_cfg};
use guitarness_verify::decide::decide;
use guitarness_verify::excite::{CombWitness, ExciteConfig, Fault, cut, frame_len};
use guitarness_verify::hyp::confusion_set;
use guitarness_verify::types::{
    Abstain, ChordOutcome, ConfusionSet, Family, HypKind, Opinion, VerifyConfig, Window,
};
use serde_json::{Value, json};
use tonica_audio::EngineEvent;
use tonica_core::chart::{Chart, EventKind, Loc};
use tonica_core::gates::{GateState, GateTable};
use tonica_core::timing::Level;
use tonica_core::verdict::{Attack, JudgeOut, PulseVerdict, VerdictType, Why};
use tonica_dsp::OnsetEvent;
use tonica_session::adapter::event_index_of;
use tonica_session::reader::Tape;

const HELP: &str = "\
tonica verify: what the chord witness hears in recorded tapes (report only)

USAGE
  tonica verify TAPE... [--gates FILE] [--json] [--detalhes]

  TAPE is a tape folder (it holds tape.json) or a folder of tape folders.

OPTIONS
  --gates FILE   read the gate table from FILE (default: the built-in table, chords locked);
                 never written
  --json         machine-readable report on stdout (no player text)
  --detalhes     also print the table of counts per take and chord, and the hints

The command judges nothing: with the chord gate locked every strum is a guide. It writes
no file. A tape without chord strums gives an empty text and exit code 0.

EXIT CODES
   0  done
  20  a tape or the gate file cannot be read, or a tape has no audio
   2  bad command line
";

/// Detuned strums in one tape after which the text suggests the tuner.
const TUNE_AFTER: usize = 3;
/// A DSP-only `Ok` counts for at most this much.
const DSP_ONLY_WEIGHT: f32 = 0.5;
/// How far an onset may sit from the detector's strum onset and still be that strum, ms.
const MATCH_MS: f64 = 10.0;
/// Strum length assumed when the detector gave none, ms.
const DEFAULT_SPREAD_MS: f64 = 30.0;

const ALL_ABSTAINS: [Abstain; 9] = [
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

fn abstain_idx(a: Abstain) -> usize {
    ALL_ABSTAINS.iter().position(|x| *x == a).unwrap_or(8)
}

struct Opts {
    tapes: Vec<PathBuf>,
    gates: Option<PathBuf>,
    json: bool,
    details: bool,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Opts, String> {
        let mut o = Opts {
            tapes: Vec::new(),
            gates: None,
            json: false,
            details: false,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--json" => o.json = true,
                "--detalhes" => o.details = true,
                "--gates" => {
                    let v = it.next().ok_or("--gates needs a value")?;
                    o.gates = Some(PathBuf::from(v));
                }
                s if s.starts_with("--") => return Err(format!("unknown option '{s}'")),
                s => o.tapes.push(PathBuf::from(s)),
            }
        }
        if o.tapes.is_empty() {
            return Err("give at least one tape folder".into());
        }
        Ok(o)
    }
}

pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return 0;
    }
    match Opts::parse(args) {
        Ok(o) => go(&o),
        Err(e) => {
            eprintln!("error: {e}\n\n{HELP}");
            2
        }
    }
}

fn fail(o: &Opts, code: i32, msg: &str, pt: &str) -> i32 {
    if o.json {
        println!("{}", json!({ "ok": false, "exit": code, "error": msg }));
    } else {
        eprintln!("{pt}");
        eprintln!("({msg})");
    }
    code
}

/// Tape folders named by `paths`: a folder with `tape.json` is a tape, any other folder
/// contributes its sub-folders that are tapes.
fn find_tapes(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for p in paths {
        if p.join("tape.json").is_file() {
            out.push(p.clone());
            continue;
        }
        let rd = std::fs::read_dir(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut subs: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|d| d.path())
            .filter(|d| d.join("tape.json").is_file())
            .collect();
        subs.sort();
        if subs.is_empty() {
            return Err(format!("{}: no tape here", p.display()));
        }
        out.extend(subs);
    }
    Ok(out)
}

/// What the witness said about one strum.
#[derive(Clone, Debug)]
enum Read {
    Supports,
    /// `Names`: the labels in the mask, and the one hypothesis when the mask holds exactly
    /// one.
    Names {
        labels: Vec<String>,
        single: Option<HypKind>,
    },
    Abstain(Abstain),
    /// The guitar is outside the tuning range of the witness: no right, no wrong.
    Detuned,
}

/// How the event ends up, after `decide` (one witness) and the gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    /// A guide only: the chord gate is locked.
    Guide,
    /// The chord was recognised (DSP only; weight at most one half).
    Ok,
    /// Chord level "does not match". Needs two witnesses, so it does not occur today.
    Mismatch,
    /// No reading (an abstention) while the gate would allow one.
    NoReading,
    /// Detuned: neither right nor wrong.
    Detuned,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Status::Guide => "guia",
            Status::Ok => "ok",
            Status::Mismatch => "nao_confere",
            Status::NoReading => "sem_leitura",
            Status::Detuned => "desafinado",
        }
    }
}

struct Rec {
    tape: usize,
    round: usize,
    id: u32,
    take: String,
    chord: String,
    /// Onset, input frame.
    at_in: f64,
    read: Read,
    status: Status,
    weight: f32,
    tune_ok: bool,
    offset_cents: f32,
    snr_db: f32,
}

#[derive(Default)]
struct Skipped {
    /// No heard onset for the event.
    no_onset: usize,
    /// A strum with no closed set (no required string, unknown voicing).
    no_set: usize,
    /// The audio window could not be read.
    no_audio: usize,
}

struct TapeOut {
    path: PathBuf,
    complete: bool,
    rounds: usize,
    recs: Vec<Rec>,
    /// Events read (`recs` is moved out when the report is built).
    n_events: usize,
    skipped: Skipped,
    detuned: usize,
    notes: Vec<String>,
}

struct Round {
    chart: Option<Chart>,
    take: String,
    strict: bool,
    fin: BTreeMap<u32, PulseVerdict>,
}

/// The reading the witness gave, in the shape the report keeps.
fn read_of(op: Opinion, fault: Option<Fault>, set: &ConfusionSet) -> Read {
    if fault == Some(Fault::Detuned) {
        return Read::Detuned;
    }
    match op {
        Opinion::Supports { .. } => Read::Supports,
        Opinion::Abstains(a) => Read::Abstain(a),
        Opinion::Names { alts, .. } => {
            let hit: Vec<usize> = (0..set.alts.len().min(32))
                .filter(|i| alts & (1u32 << i) != 0)
                .collect();
            Read::Names {
                labels: hit.iter().map(|&i| label_of(&set.alts[i].kind)).collect(),
                single: (hit.len() == 1).then(|| set.alts[hit[0]].kind.clone()),
            }
        }
    }
}

fn strict_of(level: Level) -> bool {
    !matches!(level, Level::Apprentice)
}

fn read_tape(
    path: &Path,
    index: usize,
    w: &mut CombWitness,
    gates: &GateTable,
    cfg: &VerifyConfig,
) -> Result<TapeOut, String> {
    let tape = Tape::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = TapeOut {
        path: path.to_path_buf(),
        complete: tape.complete,
        rounds: 0,
        recs: Vec::new(),
        n_events: 0,
        skipped: Skipped::default(),
        detuned: 0,
        notes: Vec::new(),
    };
    if !tape.complete {
        out.notes
            .push("the tape is incomplete (damaged or the audio lost)".into());
    }

    // One pass over the tape: rounds with their last final verdicts, and the detector's
    // strums (they give the length of each stroke and the neighbours).
    let mut rounds: Vec<Round> = Vec::new();
    let mut strums: Vec<(f64, f64)> = Vec::new();
    let mut take = "-".to_string();
    for (_, ev) in &tape.events {
        use tonica_session::format::TapeEvent as E;
        match ev {
            E::Take { label } => take = label.clone(),
            E::Anchor {
                chart_json, judge, ..
            } => {
                let level = judge.map_or(tape.header.judge.level, |j| j.level);
                let chart = Chart::from_json(chart_json).ok();
                if chart.is_none() {
                    out.notes.push("a round's chart could not be read".into());
                }
                rounds.push(Round {
                    chart,
                    take: take.clone(),
                    strict: strict_of(level),
                    fin: BTreeMap::new(),
                });
            }
            E::Out(JudgeOut::Verdict(v)) if v.fin => {
                if let Some(r) = rounds.last_mut() {
                    r.fin.insert(v.id, *v);
                }
            }
            E::Out(JudgeOut::Withdrawn { id, why }) if *why != Why::Disputed => {
                if let Some(r) = rounds.last_mut() {
                    r.fin.remove(id);
                }
            }
            E::Engine(EngineEvent::Onset(n)) => {
                if let OnsetEvent::Strum(s) = n.event
                    && n.t_in.is_finite()
                {
                    strums.push((n.t_in, s.spread.max(0.0)));
                }
            }
            _ => {}
        }
    }
    strums.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.rounds = rounds.len();

    let fs = f64::from(tape.header.fs_in);
    let ms = |t: f64| (t * 1e-3 * fs).round().max(0.0) as u64;
    let frame = frame_len(fs) as u64;
    for (ri, r) in rounds.iter().enumerate() {
        let Some(chart) = &r.chart else { continue };
        for (&id, v) in &r.fin {
            let Some(idx) = event_index_of(chart, id) else {
                continue;
            };
            let ev = &chart.events()[idx];
            if !matches!(ev.kind, EventKind::Strum { .. }) {
                continue;
            }
            let onset = match (v.attack, v.onset_in) {
                (Attack::Heard, Some(t)) if t.is_finite() && t >= 0.0 => t,
                _ => {
                    out.skipped.no_onset += 1;
                    continue;
                }
            };
            let Some(set) = confusion_set(
                chart,
                Loc {
                    tier: 0,
                    index: idx,
                },
                r.strict,
            ) else {
                out.skipped.no_set += 1;
                continue;
            };
            // The detector's strum nearest to the matched onset.
            let near = strums
                .iter()
                .filter(|(t, _)| (t - onset).abs() <= ms(MATCH_MS) as f64)
                .min_by(|a, b| (a.0 - onset).abs().total_cmp(&(b.0 - onset).abs()));
            let (at, spread) = near.map_or((onset, ms(DEFAULT_SPREAD_MS) as f64), |&(t, s)| (t, s));
            let attack = at.round() as u64;
            let last = attack + spread.round() as u64;
            let prev_end = strums
                .iter()
                .filter(|(t, _)| *t < at - 1.0)
                .map(|(t, s)| (t + s).round() as u64)
                .max()
                .unwrap_or(0);
            let next_attack = strums
                .iter()
                .find(|(t, _)| *t > at + 1.0)
                .map(|(t, _)| t.round() as u64);
            let from = attack.saturating_sub(frame + 2 * ms(5.0));
            let to = last + ms(10.0) + frame + ms(5.0);
            let audio = match tape.audio(from, to) {
                Ok(a) => a,
                Err(e) => {
                    out.skipped.no_audio += 1;
                    out.notes.push(format!("audio window: {e}"));
                    continue;
                }
            };
            let loc = |x: u64| x.saturating_sub(from) as usize;
            let win: Window = cut(
                &audio,
                fs,
                loc(attack),
                loc(last),
                loc(prev_end),
                next_attack.map_or(audio.len(), |n| loc(n).min(audio.len())),
            );
            let a = *w.analyze(&win, &set);
            let read = read_of(a.opinion, a.fault, &set);
            let (status, weight) = if matches!(read, Read::Detuned) {
                out.detuned += 1;
                (Status::Detuned, 0.0)
            } else {
                // One witness only: `decide` never accuses, it can only approve or abstain.
                let verdict = decide(&set, &[(Family::Dsp, a.opinion)], gates, cfg);
                match verdict.outcome {
                    ChordOutcome::Ok => (Status::Ok, DSP_ONLY_WEIGHT),
                    ChordOutcome::Wrong(_) => (Status::Mismatch, 0.0),
                    ChordOutcome::NotJudged(Abstain::GateLocked) => (Status::Guide, 0.0),
                    ChordOutcome::NotJudged(_) => (Status::NoReading, 0.0),
                }
            };
            out.recs.push(Rec {
                tape: index,
                round: ri,
                id,
                take: r.take.clone(),
                chord: ev.chord.clone().unwrap_or_default(),
                at_in: onset,
                read,
                status,
                weight,
                tune_ok: a.tune_ok,
                offset_cents: a.offset_cents,
                snr_db: a.snr_db,
            });
        }
    }
    Ok(out)
}

/// Counts for one (take, chord).
#[derive(Default)]
struct Pair {
    n: usize,
    supports: usize,
    names: usize,
    abstain: [usize; 9],
    detuned: usize,
    tune_off: usize,
    ok: usize,
    ok_weight: f32,
    named: BTreeMap<String, usize>,
}

fn pairs_of(recs: &[Rec]) -> BTreeMap<(String, String), Pair> {
    let mut m: BTreeMap<(String, String), Pair> = BTreeMap::new();
    for r in recs {
        let p = m.entry((r.take.clone(), r.chord.clone())).or_default();
        p.n += 1;
        if !r.tune_ok {
            p.tune_off += 1;
        }
        match &r.read {
            Read::Supports => p.supports += 1,
            Read::Names { labels, .. } => {
                p.names += 1;
                for l in labels {
                    *p.named.entry(l.clone()).or_default() += 1;
                }
            }
            Read::Abstain(a) => p.abstain[abstain_idx(*a)] += 1,
            Read::Detuned => p.detuned += 1,
        }
        if r.status == Status::Ok {
            p.ok += 1;
            p.ok_weight += r.weight;
        }
    }
    m
}

/// A hint: the same single `WrongFret` or `Extra` on two or more consecutive strums of one
/// chord in one round. A measurement, never a verdict.
struct Hint {
    take: String,
    chord: String,
    kind: String,
    string: u8,
    run: usize,
    first_id: u32,
}

fn hints_of(recs: &[Rec]) -> Vec<Hint> {
    let mut out = Vec::new();
    let key = |r: &Rec| -> Option<(HypKind, String)> {
        if !r.tune_ok {
            return None;
        }
        match &r.read {
            Read::Names {
                single: Some(k @ (HypKind::WrongFret { .. } | HypKind::Extra { .. })),
                ..
            } => Some((k.clone(), label_of(k))),
            _ => None,
        }
    };
    let mut i = 0;
    while i < recs.len() {
        let Some((kind, label)) = key(&recs[i]) else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < recs.len()
            && recs[j].tape == recs[i].tape
            && recs[j].round == recs[i].round
            && recs[j].chord == recs[i].chord
            && key(&recs[j]).is_some_and(|(k, _)| k == kind)
        {
            j += 1;
        }
        if j - i >= 2 {
            let string = match kind {
                HypKind::WrongFret { string, .. } | HypKind::Extra { string } => string,
                _ => 0,
            };
            out.push(Hint {
                take: recs[i].take.clone(),
                chord: recs[i].chord.clone(),
                kind: label,
                string,
                run: j - i,
                first_id: recs[i].id,
            });
        }
        i = j.max(i + 1);
    }
    out
}

fn go(o: &Opts) -> i32 {
    let paths = match find_tapes(&o.tapes) {
        Ok(p) => p,
        Err(e) => return fail(o, 20, &e, "Não achei fita nesse caminho."),
    };
    let gates = match &o.gates {
        Some(f) if f.is_file() => match std::fs::read_to_string(f)
            .map_err(|e| e.to_string())
            .and_then(|s| GateTable::from_json(&s).map_err(|e| format!("{e:?}")))
        {
            Ok(g) => g,
            Err(e) => {
                return fail(
                    o,
                    20,
                    &format!("{}: {e}", f.display()),
                    "Não consegui ler o arquivo de portões.",
                );
            }
        },
        _ => GateTable::builtin(),
    };
    let exc = ExciteConfig::default();
    let mut cfg = worst_case_cfg(&exc);
    // No tape-derived margin exists, and one witness cannot accuse anyway.
    cfg.accuse_margin = None;
    let mut w = CombWitness::new(exc);

    let mut tapes = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        match read_tape(p, i, &mut w, &gates, &cfg) {
            Ok(t) => tapes.push(t),
            Err(e) => return fail(o, 20, &e, "Uma fita não pôde ser lida."),
        }
    }
    let mut all: Vec<Rec> = Vec::new();
    for t in &mut tapes {
        t.n_events = t.recs.len();
        all.append(&mut t.recs);
    }
    let locked = gates.state(VerdictType::ChordIdentity) == GateState::Locked;
    let suggest_tune = tapes.iter().any(|t| t.detuned >= TUNE_AFTER);
    let pairs = pairs_of(&all);
    let hints = hints_of(&all);

    if o.json {
        println!(
            "{}",
            serde_json::to_string(&json_of(&tapes, &all, &pairs, &hints, locked, suggest_tune))
                .unwrap_or_else(|_| "{}".into())
        );
    } else {
        print!("{}", text_of(&all, &pairs, locked, suggest_tune));
        if o.details && !all.is_empty() {
            print!("{}", details_of(&tapes, &pairs, &hints));
        }
    }
    0
}

fn share_ge(k: usize, n: usize, num: usize, den: usize) -> bool {
    n > 0 && k * den >= n * num
}

/// Player text: no numbers (chord symbols excepted); empty when there was nothing to read.
fn text_of(
    recs: &[Rec],
    pairs: &BTreeMap<(String, String), Pair>,
    locked: bool,
    suggest_tune: bool,
) -> String {
    if recs.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    if locked {
        s.push_str("Os acordes ainda não valem nota; isto é só guia.\n");
    } else {
        s.push_str("Leitura dos acordes só pelo áudio; serve de guia.\n");
    }
    // One line per chord, whatever the take.
    let mut by_chord: BTreeMap<&str, Pair> = BTreeMap::new();
    for ((_, chord), p) in pairs {
        let q = by_chord.entry(chord.as_str()).or_default();
        q.n += p.n;
        q.supports += p.supports;
        q.names += p.names;
        q.detuned += p.detuned;
        q.tune_off += p.tune_off;
        q.ok += p.ok;
    }
    for (chord, p) in &by_chord {
        let judged = p.n - p.detuned;
        let line = if share_ge(p.supports, judged, 4, 5) {
            format!("{chord}: soou como {chord}, quase sempre.")
        } else if share_ge(p.supports, judged, 1, 2) {
            format!("{chord}: soou como {chord}, na maioria das vezes.")
        } else if judged > 0 && p.names * 2 > judged && p.tune_off == 0 {
            format!("{chord}: o acorde não confere com o pedido (só guia).")
        } else if judged == 0 {
            format!("{chord}: a afinação saiu do alcance; sem leitura.")
        } else {
            format!("{chord}: ainda sem leitura firme.")
        };
        s.push_str(&line);
        s.push('\n');
    }
    if suggest_tune {
        s.push_str("A afinação saiu do alcance em vários golpes; rode `tonica tune`.\n");
    }
    s
}

fn details_of(
    tapes: &[TapeOut],
    pairs: &BTreeMap<(String, String), Pair>,
    hints: &[Hint],
) -> String {
    use std::fmt::Write as _;
    let mut s = String::from("\n");
    let _ = writeln!(
        s,
        "take | acorde | n | apoia | nomeia | baixo_sinal | curto | inobservavel | ambiguo | outro | desafinado | sem_afinacao | ok (peso)"
    );
    for ((take, chord), p) in pairs {
        let other = p.abstained()
            - p.abstain[abstain_idx(Abstain::LowSnr)]
            - p.abstain[abstain_idx(Abstain::TooShort)]
            - p.abstain[abstain_idx(Abstain::Unobservable)]
            - p.abstain[abstain_idx(Abstain::Ambiguous)];
        let _ = writeln!(
            s,
            "{take} | {chord} | {} | {} | {} | {} | {} | {} | {} | {other} | {} | {} | {} ({:.1})",
            p.n,
            p.supports,
            p.names,
            p.abstain[abstain_idx(Abstain::LowSnr)],
            p.abstain[abstain_idx(Abstain::TooShort)],
            p.abstain[abstain_idx(Abstain::Unobservable)],
            p.abstain[abstain_idx(Abstain::Ambiguous)],
            p.detuned,
            p.tune_off,
            p.ok,
            p.ok_weight,
        );
        if !p.named.is_empty() {
            let named: Vec<String> = p.named.iter().map(|(k, v)| format!("{k} x{v}")).collect();
            let _ = writeln!(s, "  nomeou: {}", named.join("; "));
        }
    }
    for h in hints {
        let _ = writeln!(
            s,
            "dica (medida, não é veredito): {} em {} ({}), {} golpes seguidos, a partir do evento {}",
            h.kind, h.chord, h.take, h.run, h.first_id
        );
    }
    for t in tapes {
        let sk = &t.skipped;
        let _ = writeln!(
            s,
            "fita {}: rodadas {}, sem onset {}, sem conjunto {}, sem áudio {}, desafinados {}{}",
            t.path.display(),
            t.rounds,
            sk.no_onset,
            sk.no_set,
            sk.no_audio,
            t.detuned,
            if t.complete { "" } else { " (incompleta)" }
        );
    }
    s
}

impl Pair {
    fn abstained(&self) -> usize {
        self.abstain.iter().sum()
    }
}

fn json_of(
    tapes: &[TapeOut],
    recs: &[Rec],
    pairs: &BTreeMap<(String, String), Pair>,
    hints: &[Hint],
    locked: bool,
    suggest_tune: bool,
) -> Value {
    let pair_rows: Vec<Value> = pairs
        .iter()
        .map(|((take, chord), p)| {
            let abstain: serde_json::Map<String, Value> = ALL_ABSTAINS
                .iter()
                .map(|a| {
                    (
                        abstain_name(*a).to_string(),
                        json!(p.abstain[abstain_idx(*a)]),
                    )
                })
                .collect();
            json!({
                "take": take, "chord": chord, "n": p.n, "supports": p.supports,
                "names": p.names, "abstain": abstain, "detuned": p.detuned,
                "tune_off": p.tune_off, "ok": p.ok, "ok_weight": p.ok_weight,
                "named": p.named,
            })
        })
        .collect();
    let events: Vec<Value> = recs
        .iter()
        .map(|r| {
            let read = match &r.read {
                Read::Supports => json!({"kind": "supports"}),
                Read::Names { labels, .. } => json!({"kind": "names", "named": labels}),
                Read::Abstain(a) => json!({"kind": "abstain", "why": abstain_name(*a)}),
                Read::Detuned => json!({"kind": "detuned"}),
            };
            json!({
                "tape": r.tape, "round": r.round, "id": r.id, "take": r.take,
                "chord": r.chord, "onset_in": r.at_in, "read": read,
                "status": r.status.name(), "judged": matches!(r.status, Status::Ok | Status::Mismatch),
                "weight": r.weight, "tune_ok": r.tune_ok,
                "offset_cents": r.offset_cents, "snr_db": r.snr_db,
            })
        })
        .collect();
    let hint_rows: Vec<Value> = hints
        .iter()
        .map(|h| {
            json!({
                "take": h.take, "chord": h.chord, "kind": h.kind, "string": h.string,
                "run": h.run, "first_id": h.first_id, "verdict": false,
            })
        })
        .collect();
    let tape_rows: Vec<Value> = tapes
        .iter()
        .map(|t| {
            json!({
                "path": t.path.display().to_string(), "complete": t.complete,
                "rounds": t.rounds, "events": t.n_events, "detuned": t.detuned,
                "skipped": {
                    "no_onset": t.skipped.no_onset, "no_set": t.skipped.no_set,
                    "no_audio": t.skipped.no_audio,
                },
                "notes": t.notes,
            })
        })
        .collect();
    json!({
        "ok": true,
        "chord_gate_locked": locked,
        "judged": false,
        "events_total": recs.len(),
        "suggest_tune": suggest_tune,
        "tapes": tape_rows,
        "pairs": pair_rows,
        "hints": hint_rows,
        "events": events,
    })
}
