//! Append-only `learn.jsonl` storage (the verifier/learner design plan §2.7, §3). One JSON object per line,
//! field `k` names the record. Written by hand with `serde_json::Value` (no serde derive).
//!
//! `Guest` and `Store::memory()` never touch the filesystem. Nothing here uses the network.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use guitarness_core::gates::GateState;
use guitarness_core::pulse::Exercise;
use guitarness_core::verdict::{VerdictType, Why};
use serde_json::{Map, Value, json};

use crate::display::DisplayLedger;
use crate::types::{
    Axis, BlockKind, BlockPlan, BlockRecord, BlockSpec, Candidate, Choice, Evidence, LogLine,
    Offer, Outcome, ProfileId, Provenance, ShadowLine, SkillKey, ZeroWhy,
};

pub const FORMAT_V: u32 = 1;
const POLICY: &str = "rules-v1";

// ---------------------------------------------------------------- names

fn exercise_name(e: Exercise) -> &'static str {
    match e {
        Exercise::OnClickQuarters => "quarters",
        Exercise::OnClickEighths => "eighths",
    }
}
fn exercise_parse(s: &str) -> Option<Exercise> {
    match s {
        "quarters" => Some(Exercise::OnClickQuarters),
        "eighths" => Some(Exercise::OnClickEighths),
        _ => None,
    }
}

const KINDS: [(BlockKind, &str); 6] = [
    (BlockKind::Probe, "probe"),
    (BlockKind::Familiar, "familiar"),
    (BlockKind::Recover, "recover"),
    (BlockKind::Review, "review"),
    (BlockKind::Stretch, "stretch"),
    (BlockKind::Free, "free"),
];
const GATES: [(GateState, &str); 3] = [
    (GateState::Locked, "locked"),
    (GateState::Provisional, "provisional"),
    (GateState::Open, "open"),
];
const VTYPES: [(VerdictType, &str); 10] = [
    (VerdictType::Attack, "attack"),
    (VerdictType::Timing, "timing"),
    (VerdictType::Tendency, "tendency"),
    (VerdictType::Direction, "direction"),
    (VerdictType::Accent, "accent"),
    (VerdictType::Muted, "muted"),
    (VerdictType::ChordIdentity, "chord_identity"),
    (VerdictType::StringMissing, "string_missing"),
    (VerdictType::StringExtra, "string_extra"),
    (VerdictType::WrongFret, "wrong_fret"),
];
const WHYS: [(Why, &str); 7] = [
    (Why::NoCalibration, "no_calibration"),
    (Why::ClockNotLocked, "clock_not_locked"),
    (Why::Glitch, "glitch"),
    (Why::GateLocked, "gate_locked"),
    (Why::Disputed, "disputed"),
    (Why::Ghost, "ghost"),
    (Why::NoAttack, "no_attack"),
];
const CHOICES: [(Choice, &str); 3] = [
    (Choice::Accepted, "accepted"),
    (Choice::SomethingElse, "something_else"),
    (Choice::ProbeSkipped, "probe_skipped"),
];

fn name_of<T: Copy + PartialEq>(t: &[(T, &'static str)], v: T) -> &'static str {
    t.iter().find(|(x, _)| *x == v).map_or("?", |(_, n)| n)
}
fn parse_of<T: Copy>(t: &[(T, &'static str)], s: &str) -> Option<T> {
    t.iter().find(|(_, n)| *n == s).map(|(x, _)| *x)
}

fn why_of_outcome(o: Outcome) -> Value {
    match o {
        Outcome::Success | Outcome::Failure => Value::Null,
        Outcome::Zero(ZeroWhy::NotHeard) => json!("not_heard"),
        Outcome::Zero(ZeroWhy::Disputed) => json!("disputed"),
        // `nj_` keeps `NotJudged(Disputed)` distinct from `Zero(Disputed)`.
        Outcome::Zero(ZeroWhy::NotJudged(w)) => json!(format!("nj_{}", name_of(&WHYS, w))),
    }
}

fn outcome_parse(o: &str, why: Option<&str>) -> Option<Outcome> {
    match o {
        "s" => Some(Outcome::Success),
        "f" => Some(Outcome::Failure),
        "0" => match why? {
            "not_heard" => Some(Outcome::Zero(ZeroWhy::NotHeard)),
            "disputed" => Some(Outcome::Zero(ZeroWhy::Disputed)),
            w => Some(Outcome::Zero(ZeroWhy::NotJudged(parse_of(
                &WHYS,
                w.strip_prefix("nj_")?,
            )?))),
        },
        _ => None,
    }
}

// ---------------------------------------------------------------- encoding

/// Finite `f64` as a JSON number; non-finite as `null` (JSON has no NaN or infinity).
fn num(x: f64) -> Value {
    if x.is_finite() { json!(x) } else { Value::Null }
}
fn opt_f32(x: Option<f32>) -> Value {
    x.map_or(Value::Null, |v| num(f64::from(v)))
}

fn spec_json(s: &BlockSpec) -> Value {
    json!({ "ex": exercise_name(s.exercise), "bpm": s.bpm, "bars": s.bars })
}

fn evidence_json(e: &Evidence) -> Value {
    let o = match e.outcome {
        Outcome::Success => "s",
        Outcome::Failure => "f",
        Outcome::Zero(_) => "0",
    };
    json!({
        "id": e.id,
        "skill": e.skill.0,
        "vt": name_of(&VTYPES, e.vtype),
        "g": name_of(&GATES, e.gate),
        "o": o,
        "why": why_of_outcome(e.outcome),
        "dt_ms": opt_f32(e.dt_ms),
    })
}

/// Write an `f64` field as a readable number plus `<name>_bits` (the exact IEEE bits). The
/// default `serde_json` parser can be one ulp off, so reading prefers the bits.
fn put_f64(m: &mut Map<String, Value>, name: &str, x: f64) {
    m.insert(name.to_string(), num(x));
    m.insert(format!("{name}_bits"), json!(x.to_bits()));
}

fn candidate_json(c: &Candidate) -> Value {
    let mut m = Map::new();
    m.insert("spec".into(), spec_json(&c.plan.spec));
    m.insert("kind".into(), json!(name_of(&KINDS, c.plan.kind)));
    m.insert("skill".into(), json!(c.plan.skill.0));
    m.insert("show_summary".into(), json!(c.plan.show_summary));
    put_f64(&mut m, "p", c.p);
    match c.p_hat {
        Some(x) => put_f64(&mut m, "p_hat", x),
        None => {
            m.insert("p_hat".into(), Value::Null);
        }
    }
    Value::Object(m)
}

/// One log line as a JSON object (no trailing newline).
pub fn encode_line(l: &LogLine) -> String {
    let v = match l {
        LogLine::Block(b) => json!({
            "k": "block",
            "ts_ms": b.ts_ms,
            "dur_ms": b.dur_ms,
            "session": b.session,
            "kind": name_of(&KINDS, b.kind),
            "spec": spec_json(&b.spec),
            "after_gap": b.after_gap,
            "strums": b.strums,
            "prov": {
                "judge_v": b.prov.judge_v,
                "basis": b.prov.basis,
                "gate": name_of(&GATES, b.prov.gate),
                "tape": b.prov.tape,
            },
            "ev": b.evidence.iter().map(evidence_json).collect::<Vec<_>>(),
        }),
        LogLine::Dispute { block_ts, id } => {
            json!({ "k": "dispute", "block_ts": block_ts, "id": id })
        }
        LogLine::Offer(o) => json!({
            "k": "offer",
            "policy": POLICY,
            "seed": o.seed,
            "cands": o.cands.iter().map(candidate_json).collect::<Vec<_>>(),
            "chosen": o.chosen,
            "change": o.change.map_or(Value::Null, |(a, from, to)| json!({
                "axis": match a { Axis::Tempo => "tempo", Axis::Rhythm => "rhythm" },
                "from": from,
                "to": to,
            })),
        }),
        LogLine::Choice(c) => json!({ "k": "choice", "c": name_of(&CHOICES, *c) }),
        LogLine::Shadow(s) => json!({
            "k": "shadow",
            "name": s.name,
            "skill": s.skill.0,
            "p": num(f64::from(s.p)),
            "due_ms": s.due_ms,
        }),
    };
    v.to_string()
}

// ---------------------------------------------------------------- decoding

fn get_i64(m: &Map<String, Value>, k: &str) -> Option<i64> {
    m.get(k)?.as_i64()
}
fn get_u64(m: &Map<String, Value>, k: &str) -> Option<u64> {
    m.get(k)?.as_u64()
}
fn get_u32(m: &Map<String, Value>, k: &str) -> Option<u32> {
    u32::try_from(get_u64(m, k)?).ok()
}
fn get_str<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k)?.as_str()
}
fn get_bool(m: &Map<String, Value>, k: &str) -> Option<bool> {
    m.get(k)?.as_bool()
}
fn get_obj<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a Map<String, Value>> {
    m.get(k)?.as_object()
}
/// Exact bits when `<k>_bits` is present; otherwise the number, with `null` read as NaN.
fn get_f64(m: &Map<String, Value>, k: &str) -> Option<f64> {
    if let Some(b) = m.get(&format!("{k}_bits")) {
        return Some(f64::from_bits(b.as_u64()?));
    }
    match m.get(k)? {
        Value::Null => Some(f64::NAN),
        v => v.as_f64(),
    }
}

fn spec_parse(v: &Value) -> Option<BlockSpec> {
    let m = v.as_object()?;
    Some(BlockSpec {
        exercise: exercise_parse(get_str(m, "ex")?)?,
        bpm: u16::try_from(get_u64(m, "bpm")?).ok()?,
        bars: u8::try_from(get_u64(m, "bars")?).ok()?,
    })
}

fn evidence_parse(v: &Value) -> Option<Evidence> {
    let m = v.as_object()?;
    let dt_ms = match m.get("dt_ms")? {
        Value::Null => None,
        x => Some(x.as_f64()? as f32),
    };
    let why = match m.get("why")? {
        Value::Null => None,
        x => Some(x.as_str()?),
    };
    Some(Evidence {
        id: get_u32(m, "id")?,
        skill: SkillKey(get_str(m, "skill")?.to_string()),
        vtype: parse_of(&VTYPES, get_str(m, "vt")?)?,
        outcome: outcome_parse(get_str(m, "o")?, why)?,
        gate: parse_of(&GATES, get_str(m, "g")?)?,
        dt_ms,
    })
}

fn candidate_parse(v: &Value) -> Option<Candidate> {
    let m = v.as_object()?;
    let p_hat = match m.get("p_hat")? {
        Value::Null if !m.contains_key("p_hat_bits") => None,
        _ => Some(get_f64(m, "p_hat")?),
    };
    Some(Candidate {
        plan: BlockPlan {
            spec: spec_parse(m.get("spec")?)?,
            kind: parse_of(&KINDS, get_str(m, "kind")?)?,
            skill: SkillKey(get_str(m, "skill")?.to_string()),
            show_summary: get_bool(m, "show_summary")?,
        },
        p: get_f64(m, "p")?,
        p_hat,
    })
}

/// Result of reading one line.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    Line(LogLine),
    /// Valid JSON object with a `k` this version does not know.
    Unknown,
    /// Not JSON, not an object, no `k`, or a known `k` with missing or ill-typed fields.
    Bad,
}

pub fn decode_line(s: &str) -> Decoded {
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(s) else {
        return Decoded::Bad;
    };
    let Some(k) = get_str(&m, "k") else {
        return Decoded::Bad;
    };
    let parsed = match k {
        "block" => decode_block(&m).map(LogLine::Block),
        "dispute" => (|| {
            Some(LogLine::Dispute {
                block_ts: get_i64(&m, "block_ts")?,
                id: get_u32(&m, "id")?,
            })
        })(),
        "offer" => decode_offer(&m).map(LogLine::Offer),
        "choice" => parse_of(&CHOICES, get_str(&m, "c").unwrap_or("")).map(LogLine::Choice),
        "shadow" => (|| {
            let p = get_f64(&m, "p")? as f32;
            let due_ms = match m.get("due_ms")? {
                Value::Null => None,
                x => Some(x.as_i64()?),
            };
            Some(LogLine::Shadow(ShadowLine {
                name: get_str(&m, "name")?.to_string(),
                skill: SkillKey(get_str(&m, "skill")?.to_string()),
                p,
                due_ms,
            }))
        })(),
        _ => return Decoded::Unknown,
    };
    parsed.map_or(Decoded::Bad, Decoded::Line)
}

fn decode_block(m: &Map<String, Value>) -> Option<BlockRecord> {
    let prov = get_obj(m, "prov")?;
    let tape = match prov.get("tape")? {
        Value::Null => None,
        x => Some(x.as_str()?.to_string()),
    };
    let evidence = m
        .get("ev")?
        .as_array()?
        .iter()
        .map(evidence_parse)
        .collect::<Option<Vec<_>>>()?;
    Some(BlockRecord {
        ts_ms: get_i64(m, "ts_ms")?,
        dur_ms: get_u32(m, "dur_ms")?,
        session: get_u32(m, "session")?,
        kind: parse_of(&KINDS, get_str(m, "kind")?)?,
        spec: spec_parse(m.get("spec")?)?,
        after_gap: get_bool(m, "after_gap")?,
        strums: get_u32(m, "strums")?,
        evidence,
        prov: Provenance {
            judge_v: get_str(prov, "judge_v")?.to_string(),
            basis: get_str(prov, "basis")?.to_string(),
            gate: parse_of(&GATES, get_str(prov, "gate")?)?,
            tape,
        },
    })
}

fn decode_offer(m: &Map<String, Value>) -> Option<Offer> {
    let cands = m
        .get("cands")?
        .as_array()?
        .iter()
        .map(candidate_parse)
        .collect::<Option<Vec<_>>>()?;
    let change = match m.get("change")? {
        Value::Null => None,
        v => {
            let c = v.as_object()?;
            let axis = match get_str(c, "axis")? {
                "tempo" => Axis::Tempo,
                "rhythm" => Axis::Rhythm,
                _ => return None,
            };
            Some((
                axis,
                u16::try_from(get_u64(c, "from")?).ok()?,
                u16::try_from(get_u64(c, "to")?).ok()?,
            ))
        }
    };
    Some(Offer {
        cands,
        chosen: usize::try_from(get_u64(m, "chosen")?).ok()?,
        change,
        seed: get_u64(m, "seed")?,
    })
}

// ---------------------------------------------------------------- store

/// The learner log: in memory (`Guest`) or a `learn.jsonl` file (`Local`).
pub struct Store {
    lines: Vec<LogLine>,
    dir: Option<PathBuf>,
    file: Option<File>,
    unknown: usize,
    bad: usize,
    cut_bytes: usize,
}

impl Store {
    /// In-memory store: no filesystem call, ever.
    pub fn memory() -> Self {
        Self {
            lines: Vec::new(),
            dir: None,
            file: None,
            unknown: 0,
            bad: 0,
            cut_bytes: 0,
        }
    }

    /// Open (creating if needed) `<dir>/learn.jsonl`. A file that does not end in a newline
    /// (a cut write) is read up to its last whole line and the tail is removed so the next
    /// append starts on a fresh line. Unknown `k` lines are counted; unreadable lines are
    /// counted and skipped.
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join("learn.jsonl");
        let mut s = Self::memory();
        s.dir = Some(dir.to_path_buf());
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        let whole = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        s.cut_bytes = bytes.len() - whole;
        for raw in bytes[..whole].split(|&b| b == b'\n') {
            if raw.is_empty() {
                continue;
            }
            match std::str::from_utf8(raw).map(decode_line) {
                Ok(Decoded::Line(l)) => s.lines.push(l),
                Ok(Decoded::Unknown) => s.unknown += 1,
                _ => s.bad += 1,
            }
        }
        let f = OpenOptions::new().create(true).append(true).open(&path)?;
        if s.cut_bytes > 0 {
            f.set_len(whole as u64)?;
        }
        s.file = Some(f);
        Ok(s)
    }

    /// Append one line and flush it to the file (if any), then keep it in memory.
    pub fn append(&mut self, l: &LogLine) -> io::Result<()> {
        if let Some(f) = self.file.as_mut() {
            let mut s = encode_line(l);
            s.push('\n');
            f.write_all(s.as_bytes())?;
            f.flush()?;
        }
        self.lines.push(l.clone());
        Ok(())
    }

    pub fn lines(&self) -> &[LogLine] {
        &self.lines
    }
    /// Lines whose `k` this version does not know (kept in the file, not in `lines`).
    pub fn unknown_lines(&self) -> usize {
        self.unknown
    }
    /// Lines that could not be read (kept in the file, not in `lines`).
    pub fn bad_lines(&self) -> usize {
        self.bad
    }
    /// Bytes of an unfinished last line removed on open.
    pub fn truncated_bytes(&self) -> usize {
        self.cut_bytes
    }
    pub fn is_persistent(&self) -> bool {
        self.dir.is_some()
    }

    /// Save `display.json` next to the log (atomic rename). No-op for a memory store.
    pub fn save_display(&self, d: &DisplayLedger) -> io::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let tmp = dir.join("display.json.tmp");
        fs::write(&tmp, d.to_json())?;
        fs::rename(&tmp, dir.join("display.json"))
    }

    /// Read `display.json`; `None` when absent, unreadable, or for a memory store.
    pub fn load_display(&self) -> Option<DisplayLedger> {
        let dir = self.dir.as_ref()?;
        DisplayLedger::from_json(&fs::read_to_string(dir.join("display.json")).ok()?)
    }
}

// ---------------------------------------------------------------- profiles

fn mix(seed: u64) -> u64 {
    // SplitMix64 finalizer: spreads a small caller seed over 64 bits. Not secret, not random.
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `Local` id (16 lowercase hex) from a caller-supplied seed. No RNG, no clock.
pub fn local_id_from_seed(seed: u64) -> ProfileId {
    ProfileId::Local(format!("{:016x}", mix(seed)))
}

fn valid_local_id(s: &str) -> bool {
    s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `<data>/profiles/<id>`; `None` for `Guest` and for a malformed id (never builds a path from it).
pub fn profile_dir(data: &Path, id: &ProfileId) -> Option<PathBuf> {
    match id {
        ProfileId::Guest => None,
        ProfileId::Local(s) if valid_local_id(s) => Some(data.join("profiles").join(s)),
        ProfileId::Local(_) => None,
    }
}

/// Create a `Local` profile: `<data>/profiles/<id>/profile.json` = `{ v, id, created_ms }` and
/// nothing else. `AlreadyExists` if that id is taken (pick another seed).
pub fn create_local(data: &Path, seed: u64, created_ms: i64) -> io::Result<ProfileId> {
    let id = local_id_from_seed(seed);
    let dir = profile_dir(data, &id).expect("generated id is valid");
    fs::create_dir_all(dir.parent().expect("has parent"))?;
    fs::create_dir(&dir).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            io::Error::new(e.kind(), "profile id already exists")
        } else {
            e
        }
    })?;
    let ProfileId::Local(s) = &id else {
        unreachable!()
    };
    let v = json!({ "v": FORMAT_V, "id": s, "created_ms": created_ms });
    fs::write(dir.join("profile.json"), v.to_string())?;
    Ok(id)
}

/// Open the store of a profile. `Guest` returns a memory store without any filesystem call.
pub fn open_profile(data: &Path, id: &ProfileId) -> io::Result<Store> {
    match id {
        ProfileId::Guest => Ok(Store::memory()),
        ProfileId::Local(_) => {
            let dir = profile_dir(data, id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "malformed profile id")
            })?;
            Store::open(&dir)
        }
    }
}
