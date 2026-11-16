//! Tape format v1: line types and JSON encoding (owned by S1).
//!
//! A tape is a folder (plan §3):
//!
//! | file | content |
//! |---|---|
//! | `tape.json` | [`TapeHeader`]: `v`, app and system, rates, ids, stamp sources, profile, `TimingRef`, `JudgeConfig`, gates, `video_offset_ms`, takes, counts, `disputed`, `complete` |
//! | `mic.wav` | mono `f32` at the input rate: the samples the detector saw, contiguous in detector time (a lost block is zero-filled and flagged in `mic.runs.json`) |
//! | `mic.runs.json` | [`MicRun`]s: `{ det_first, in_first, frames, lost }`; a new run starts when `in_first - det_first` changes or a block was lost |
//! | `events.jsonl` | one line per [`TapeEvent`], `seq` ascending, field `k` names the line type |
//!
//! # Numbers
//!
//! `serde_json` without `float_roundtrip` can be one ulp off when it parses a float, and
//! replay needs the very samples and times the live run saw. So the tape has its own
//! small JSON reader and writer ([`Json`]): `f64` and `f32` are written in the shortest
//! form that reads back exactly (Rust's `Display` / `LowerExp`), a number is kept as text
//! and parsed with `str::parse`. `NaN` and infinities become `null`; a plain `f64` field
//! reads `null` back as `NaN` (an infinity therefore comes back as `NaN`), an `Option`
//! field reads it as `None`. `-0.0` and subnormals round-trip. `base_ns` (an `i128`) is
//! written as decimal text.
//!
//! A line whose `k` is unknown is skipped and counted (`Tape::unknown_lines`); a line that
//! does not parse, or lacks a field of its type, is skipped and counted
//! (`Tape::bad_lines`). Tuner frames (`EngineEvent::Pitch`) are not taped.

use std::fmt::Write as _;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use guitarness_audio::{
    BackendEvent, ClockStep, EngineEvent, MixEvent, OnsetNote, Profile, Side, StampSource,
    StepCause,
    clock::{ClockParts, ClockQuality},
    stats::Ring,
};
use guitarness_core::gates::GateTable;
use guitarness_core::judge::JudgeConfig;
use guitarness_core::stats::{NoClaim, RoundStats, Tendency};
use guitarness_core::timing::{Level, TimingBasis, TimingRef};
use guitarness_core::verdict::{Attack, Direction, JudgeOut, PulseVerdict, Timing, Why};
use guitarness_dsp::{OnsetEvent, Strum};

use crate::rig::{HitKind, TruthStrum};

/// Version written in `v` of every tape file.
pub const TAPE_VERSION: u64 = 1;
/// Tapes are off until the player opts in (plan §12, open question 1: the owner has not
/// decided to record every session). The session reads this to set its default.
pub const TAPES_DEFAULT_ON: bool = false;
/// Proposed ceiling for the tapes folder, bytes (plan §12). Not enforced anywhere until the
/// owner decides; `prune(root, DEFAULT_MAX_BYTES)` is what a caller would run.
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------------------
// Minimal JSON
// ---------------------------------------------------------------------------------------

/// A JSON value that keeps numbers as text, so floats survive the trip exactly.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, in the text it has on disk.
    Num(String),
    /// A string (decoded).
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object, in order.
    Obj(Vec<(String, Json)>),
}

fn fmt_f64(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let a = x.abs();
    if (1e-5..1e15).contains(&a) {
        format!("{x}")
    } else {
        format!("{x:e}")
    }
}

fn fmt_f32(x: f32) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let a = x.abs();
    if (1e-5..1e15).contains(&a) {
        format!("{x}")
    } else {
        format!("{x:e}")
    }
}

impl Json {
    /// A float: shortest exact text, `null` if not finite.
    pub fn f64(x: f64) -> Json {
        if x.is_finite() {
            Json::Num(fmt_f64(x))
        } else {
            Json::Null
        }
    }

    /// An `f32`: shortest exact text (as `f32`), `null` if not finite.
    pub fn f32(x: f32) -> Json {
        if x.is_finite() {
            Json::Num(fmt_f32(x))
        } else {
            Json::Null
        }
    }

    /// `Some` as a float, `None` as `null`.
    pub fn opt_f64(x: Option<f64>) -> Json {
        x.map_or(Json::Null, Json::f64)
    }

    /// An unsigned integer.
    pub fn u(n: u64) -> Json {
        Json::Num(n.to_string())
    }

    /// A signed integer.
    pub fn i(n: i64) -> Json {
        Json::Num(n.to_string())
    }

    /// A string.
    pub fn s(text: &str) -> Json {
        Json::Str(text.to_string())
    }

    /// An object from `(key, value)` pairs.
    pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// Member `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Compact text.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    /// Text with one top-level member per line (for `tape.json`).
    pub fn to_text_pretty(&self) -> String {
        match self {
            Json::Obj(v) => {
                let mut s = String::from("{\n");
                for (i, (k, val)) in v.iter().enumerate() {
                    s.push_str("  ");
                    write_str(k, &mut s);
                    s.push_str(": ");
                    val.write(&mut s);
                    if i + 1 < v.len() {
                        s.push(',');
                    }
                    s.push('\n');
                }
                s.push('}');
                s.push('\n');
                s
            }
            other => other.to_text(),
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(t) => out.push_str(t),
            Json::Str(t) => write_str(t, out),
            Json::Arr(v) => {
                out.push('[');
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    x.write(out);
                }
                out.push(']');
            }
            Json::Obj(v) => {
                out.push('{');
                for (i, (k, x)) in v.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(k, out);
                    out.push(':');
                    x.write(out);
                }
                out.push('}');
            }
        }
    }

    /// Parses one JSON value; trailing text is an error.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser {
            b: text.as_bytes(),
            i: 0,
            depth: 0,
        };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("trailing text at byte {}", p.i));
        }
        Ok(v)
    }

    /// A `serde_json` value as a [`Json`] (numbers through their own text form).
    pub fn from_serde(v: &serde_json::Value) -> Json {
        use serde_json::Value;
        match v {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    Json::u(u)
                } else if let Some(i) = n.as_i64() {
                    Json::i(i)
                } else {
                    Json::f64(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Value::String(s) => Json::Str(s.clone()),
            Value::Array(a) => Json::Arr(a.iter().map(Json::from_serde).collect()),
            Value::Object(o) => Json::Obj(
                o.iter()
                    .map(|(k, v)| (k.clone(), Json::from_serde(v)))
                    .collect(),
            ),
        }
    }

    /// This value as a `serde_json` value (floats are parsed exactly with `str::parse`).
    pub fn to_serde(&self) -> serde_json::Value {
        use serde_json::{Number, Value};
        match self {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Num(t) => {
                if let Ok(u) = t.parse::<u64>() {
                    Value::Number(Number::from(u))
                } else if let Ok(i) = t.parse::<i64>() {
                    Value::Number(Number::from(i))
                } else {
                    t.parse::<f64>()
                        .ok()
                        .and_then(Number::from_f64)
                        .map_or(Value::Null, Value::Number)
                }
            }
            Json::Str(s) => Value::String(s.clone()),
            Json::Arr(a) => Value::Array(a.iter().map(Json::to_serde).collect()),
            Json::Obj(o) => {
                Value::Object(o.iter().map(|(k, v)| (k.clone(), v.to_serde())).collect())
            }
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

const MAX_DEPTH: u32 = 64;

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", c as char, self.i))
        }
    }

    fn lit(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(format!("bad literal at byte {}", self.i))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        match self.b.get(self.i) {
            None => Err("unexpected end".into()),
            Some(b'n') => self.lit("null", Json::Null),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b'[') => {
                self.enter()?;
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        v.push(self.value()?);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(format!("bad array at byte {}", self.i)),
                        }
                    }
                }
                self.depth -= 1;
                Ok(Json::Arr(v))
            }
            Some(b'{') => {
                self.enter()?;
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        let k = self.string()?;
                        self.ws();
                        self.eat(b':')?;
                        self.ws();
                        v.push((k, self.value()?));
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(format!("bad object at byte {}", self.i)),
                        }
                    }
                }
                self.depth -= 1;
                Ok(Json::Obj(v))
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => Err(format!("unexpected byte at {}", self.i)),
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err("too deep".into())
        } else {
            Ok(())
        }
    }

    fn digits(&mut self) -> usize {
        let st = self.i;
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        self.i - st
    }

    /// JSON number grammar: `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`.
    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        let bad = || format!("bad number at byte {start}");
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(bad()),
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if self.digits() == 0 {
                return Err(bad());
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return Err(bad());
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
        match text.parse::<f64>() {
            Ok(x) if x.is_finite() => Ok(Json::Num(text.to_string())),
            _ => Err(format!("bad number at byte {start}")),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self
            .b
            .get(self.i..self.i + 4)
            .ok_or_else(|| "short \\u escape".to_string())?;
        let t = std::str::from_utf8(h).map_err(|e| e.to_string())?;
        if !t.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("bad \\u escape".into());
        }
        self.i += 4;
        u32::from_str_radix(t, 16).map_err(|e| e.to_string())
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.b.len() && !matches!(self.b[self.i], b'"' | b'\\' | 0..=0x1f) {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?);
            match self.b.get(self.i) {
                None => return Err("unterminated string".into()),
                Some(c) if *c < 0x20 => return Err("control character in string".into()),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(_) => {
                    self.i += 1;
                    let c = *self.b.get(self.i).ok_or("bad escape")?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return Err("lone high surrogate".into());
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err("high surrogate without a low one".into());
                                }
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            } else if (0xDC00..0xE000).contains(&cp) {
                                return Err("lone low surrogate".into());
                            }
                            out.push(char::from_u32(cp).ok_or("bad code point")?);
                        }
                        _ => return Err("bad escape".into()),
                    }
                }
            }
        }
    }
}

/// Typed access to the members of one object, with error messages that name the field.
pub struct Fields<'a>(&'a Json);

impl<'a> Fields<'a> {
    /// `v` must be an object.
    pub fn new(v: &'a Json) -> Result<Self, String> {
        match v {
            Json::Obj(_) => Ok(Fields(v)),
            _ => Err("not an object".into()),
        }
    }

    /// Member `key`, or `None` if absent.
    pub fn opt_member(&self, key: &str) -> Option<&'a Json> {
        self.0.get(key)
    }

    /// Member `key` (error if absent).
    pub fn raw(&self, key: &str) -> Result<&'a Json, String> {
        self.0.get(key).ok_or_else(|| format!("missing '{key}'"))
    }

    fn num(&self, key: &str) -> Result<Option<&'a str>, String> {
        match self.raw(key)? {
            Json::Null => Ok(None),
            Json::Num(t) => Ok(Some(t)),
            _ => Err(format!("'{key}' is not a number")),
        }
    }

    /// A float; `null` reads as `NaN`.
    pub fn f64(&self, key: &str) -> Result<f64, String> {
        match self.num(key)? {
            None => Ok(f64::NAN),
            Some(t) => t.parse().map_err(|_| format!("bad '{key}'")),
        }
    }

    /// An `f32`; `null` reads as `NaN`.
    pub fn f32(&self, key: &str) -> Result<f32, String> {
        match self.num(key)? {
            None => Ok(f32::NAN),
            Some(t) => t.parse().map_err(|_| format!("bad '{key}'")),
        }
    }

    /// A float or `null`. An absent member is an error.
    pub fn opt_f64(&self, key: &str) -> Result<Option<f64>, String> {
        match self.num(key)? {
            None => Ok(None),
            Some(t) => t.parse().map(Some).map_err(|_| format!("bad '{key}'")),
        }
    }

    /// An `f32` or `null`.
    pub fn opt_f32(&self, key: &str) -> Result<Option<f32>, String> {
        match self.num(key)? {
            None => Ok(None),
            Some(t) => t.parse().map(Some).map_err(|_| format!("bad '{key}'")),
        }
    }

    /// An unsigned integer.
    pub fn u64(&self, key: &str) -> Result<u64, String> {
        match self.num(key)? {
            None => Err(format!("'{key}' is null")),
            Some(t) => t.parse().map_err(|_| format!("bad '{key}'")),
        }
    }

    /// An unsigned integer or `null`.
    pub fn opt_u64(&self, key: &str) -> Result<Option<u64>, String> {
        match self.num(key)? {
            None => Ok(None),
            Some(t) => t.parse().map(Some).map_err(|_| format!("bad '{key}'")),
        }
    }

    /// A signed integer.
    pub fn i64(&self, key: &str) -> Result<i64, String> {
        match self.num(key)? {
            None => Err(format!("'{key}' is null")),
            Some(t) => t.parse().map_err(|_| format!("bad '{key}'")),
        }
    }

    /// A `u32`.
    pub fn u32(&self, key: &str) -> Result<u32, String> {
        u32::try_from(self.u64(key)?).map_err(|_| format!("'{key}' out of range"))
    }

    /// A `u16`.
    pub fn u16(&self, key: &str) -> Result<u16, String> {
        u16::try_from(self.u64(key)?).map_err(|_| format!("'{key}' out of range"))
    }

    /// A `u8`.
    pub fn u8(&self, key: &str) -> Result<u8, String> {
        u8::try_from(self.u64(key)?).map_err(|_| format!("'{key}' out of range"))
    }

    /// A boolean.
    pub fn bool(&self, key: &str) -> Result<bool, String> {
        match self.raw(key)? {
            Json::Bool(b) => Ok(*b),
            _ => Err(format!("'{key}' is not a bool")),
        }
    }

    /// A string.
    pub fn str(&self, key: &str) -> Result<&'a str, String> {
        match self.raw(key)? {
            Json::Str(s) => Ok(s),
            _ => Err(format!("'{key}' is not a string")),
        }
    }

    /// A string or `null`.
    pub fn opt_str(&self, key: &str) -> Result<Option<&'a str>, String> {
        match self.raw(key)? {
            Json::Null => Ok(None),
            Json::Str(s) => Ok(Some(s)),
            _ => Err(format!("'{key}' is not a string")),
        }
    }

    /// A member that must be an object.
    pub fn obj(&self, key: &str) -> Result<Fields<'a>, String> {
        Fields::new(self.raw(key)?).map_err(|e| format!("'{key}': {e}"))
    }

    /// A member that must be an array.
    pub fn arr(&self, key: &str) -> Result<&'a [Json], String> {
        match self.raw(key)? {
            Json::Arr(v) => Ok(v),
            _ => Err(format!("'{key}' is not an array")),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------------------

fn why_name(w: Why) -> &'static str {
    match w {
        Why::NoCalibration => "no_calibration",
        Why::ClockNotLocked => "clock_not_locked",
        Why::Glitch => "glitch",
        Why::GateLocked => "gate_locked",
        Why::Disputed => "disputed",
        Why::Ghost => "ghost",
        Why::NoAttack => "no_attack",
    }
}

fn why_from(s: &str) -> Result<Why, String> {
    Ok(match s {
        "no_calibration" => Why::NoCalibration,
        "clock_not_locked" => Why::ClockNotLocked,
        "glitch" => Why::Glitch,
        "gate_locked" => Why::GateLocked,
        "disputed" => Why::Disputed,
        "ghost" => Why::Ghost,
        "no_attack" => Why::NoAttack,
        _ => return Err(format!("unknown why '{s}'")),
    })
}

fn attack_name(a: Attack) -> String {
    match a {
        Attack::Heard => "heard".into(),
        Attack::NotHeard => "not_heard".into(),
        Attack::NotJudged(w) => format!("nj:{}", why_name(w)),
    }
}

fn attack_from(s: &str) -> Result<Attack, String> {
    match s {
        "heard" => Ok(Attack::Heard),
        "not_heard" => Ok(Attack::NotHeard),
        _ => Ok(Attack::NotJudged(why_from(
            s.strip_prefix("nj:")
                .ok_or_else(|| format!("unknown attack '{s}'"))?,
        )?)),
    }
}

fn timing_name(t: Timing) -> String {
    match t {
        Timing::Center => "center".into(),
        Timing::InWindow => "in_window".into(),
        Timing::Outside => "outside".into(),
        Timing::NotJudged(w) => format!("nj:{}", why_name(w)),
    }
}

fn timing_from(s: &str) -> Result<Timing, String> {
    match s {
        "center" => Ok(Timing::Center),
        "in_window" => Ok(Timing::InWindow),
        "outside" => Ok(Timing::Outside),
        _ => Ok(Timing::NotJudged(why_from(
            s.strip_prefix("nj:")
                .ok_or_else(|| format!("unknown timing '{s}'"))?,
        )?)),
    }
}

fn dir_name(d: Direction) -> String {
    match d {
        Direction::NotJudged(w) => format!("nj:{}", why_name(w)),
    }
}

fn dir_from(s: &str) -> Result<Direction, String> {
    Ok(Direction::NotJudged(why_from(
        s.strip_prefix("nj:")
            .ok_or_else(|| format!("unknown direction '{s}'"))?,
    )?))
}

fn level_name(l: Level) -> &'static str {
    match l {
        Level::Apprentice => "apprentice",
        Level::Band => "band",
        Level::Stage => "stage",
    }
}

fn level_from(s: &str) -> Result<Level, String> {
    Level::ALL
        .into_iter()
        .find(|l| level_name(*l) == s)
        .ok_or_else(|| format!("unknown level '{s}'"))
}

fn quality_name(q: ClockQuality) -> &'static str {
    match q {
        ClockQuality::Acquiring => "acquiring",
        ClockQuality::Locked => "locked",
        ClockQuality::Degraded => "degraded",
        ClockQuality::Broken => "broken",
    }
}

fn quality_from(s: &str) -> Result<ClockQuality, String> {
    Ok(match s {
        "acquiring" => ClockQuality::Acquiring,
        "locked" => ClockQuality::Locked,
        "degraded" => ClockQuality::Degraded,
        "broken" => ClockQuality::Broken,
        _ => return Err(format!("unknown quality '{s}'")),
    })
}

fn source_name(s: StampSource) -> &'static str {
    match s {
        StampSource::Device => "device",
        StampSource::Callback => "callback",
    }
}

fn source_from(s: &str) -> Result<StampSource, String> {
    match s {
        "device" => Ok(StampSource::Device),
        "callback" => Ok(StampSource::Callback),
        _ => Err(format!("unknown stamp source '{s}'")),
    }
}

fn side_name(s: Side) -> &'static str {
    match s {
        Side::In => "in",
        Side::Out => "out",
    }
}

fn side_from(s: &str) -> Result<Side, String> {
    match s {
        "in" => Ok(Side::In),
        "out" => Ok(Side::Out),
        _ => Err(format!("unknown side '{s}'")),
    }
}

fn cause_name(c: StepCause) -> &'static str {
    match c {
        StepCause::Detected => "detected",
        StepCause::Notice => "notice",
        StepCause::Fallback => "fallback",
    }
}

fn cause_from(s: &str) -> Result<StepCause, String> {
    Ok(match s {
        "detected" => StepCause::Detected,
        "notice" => StepCause::Notice,
        "fallback" => StepCause::Fallback,
        _ => return Err(format!("unknown cause '{s}'")),
    })
}

fn backend_name(b: BackendEvent) -> &'static str {
    match b {
        BackendEvent::XrunIn => "xrun_in",
        BackendEvent::XrunOut => "xrun_out",
        BackendEvent::Invalidated => "invalidated",
        BackendEvent::DeviceLost => "device_lost",
        BackendEvent::DeviceChanged => "device_changed",
        BackendEvent::RealtimeDenied => "realtime_denied",
        BackendEvent::Other => "other",
    }
}

fn backend_from(s: &str) -> Result<BackendEvent, String> {
    Ok(match s {
        "xrun_in" => BackendEvent::XrunIn,
        "xrun_out" => BackendEvent::XrunOut,
        "invalidated" => BackendEvent::Invalidated,
        "device_lost" => BackendEvent::DeviceLost,
        "device_changed" => BackendEvent::DeviceChanged,
        "realtime_denied" => BackendEvent::RealtimeDenied,
        "other" => BackendEvent::Other,
        _ => return Err(format!("unknown backend event '{s}'")),
    })
}

fn ring_name(r: Ring) -> &'static str {
    match r {
        Ring::MixCmds => "mix_cmds",
        Ring::Events => "events",
        Ring::InEvents => "in_events",
        Ring::Pitch => "pitch",
        Ring::OutStamps => "out_stamps",
        Ring::MixEvents => "mix_events",
        Ring::Backend => "backend",
        Ring::Tap => "tap",
    }
}

fn ring_from(s: &str) -> Result<Ring, String> {
    Ok(match s {
        "mix_cmds" => Ring::MixCmds,
        "events" => Ring::Events,
        "in_events" => Ring::InEvents,
        "pitch" => Ring::Pitch,
        "out_stamps" => Ring::OutStamps,
        "mix_events" => Ring::MixEvents,
        "backend" => Ring::Backend,
        "tap" => Ring::Tap,
        _ => return Err(format!("unknown ring '{s}'")),
    })
}

fn kind_name(k: HitKind) -> &'static str {
    match k {
        HitKind::Strum => "strum",
        HitKind::Mute => "mute",
        HitKind::Ghost => "ghost",
    }
}

fn kind_from(s: &str) -> Result<HitKind, String> {
    Ok(match s {
        "strum" => HitKind::Strum,
        "mute" => HitKind::Mute,
        "ghost" => HitKind::Ghost,
        _ => return Err(format!("unknown hit kind '{s}'")),
    })
}

fn noclaim_name(n: NoClaim) -> &'static str {
    match n {
        NoClaim::EstimateOnly => "estimate_only",
        NoClaim::TooFew => "too_few",
        NoClaim::Uncertain => "uncertain",
        NoClaim::GateLocked => "gate_locked",
    }
}

fn noclaim_from(s: &str) -> Result<NoClaim, String> {
    Ok(match s {
        "estimate_only" => NoClaim::EstimateOnly,
        "too_few" => NoClaim::TooFew,
        "uncertain" => NoClaim::Uncertain,
        "gate_locked" => NoClaim::GateLocked,
        _ => return Err(format!("unknown no-claim '{s}'")),
    })
}

// ---------------------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------------------

/// A command the session received, as the tape records it. The handles of
/// `SessionCmd::Rewire` and the chart of `StartRound` are not kept here: the chart goes
/// to the tape in [`TapeEvent::Anchor`]. `SessionCmd::Take` is recorded by its own
/// [`TapeEvent::Take`] line.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionCmdLog {
    /// `Soundcheck { force }`.
    Soundcheck {
        /// Run the full check even with a saved profile.
        force: bool,
    },
    /// `StartRound { lead_in_beats, take, .. }`.
    StartRound {
        /// Count-in beats.
        lead_in_beats: u32,
        /// Take label, if any.
        take: Option<String>,
    },
    /// `StopRound`.
    StopRound,
    /// `Dispute`: the player contested the last verdict. A tape with this line is never
    /// pruned.
    Dispute,
    /// `SetLevel`.
    SetLevel(Level),
    /// `Record(on)`.
    Record(bool),
    /// `Rewire { .. }`.
    Rewire,
    /// `Shutdown`.
    Shutdown,
    /// `Click { bpm, beats, accent_every }`: a click track with no judge.
    Click {
        /// Tempo.
        bpm: f64,
        /// Clicks.
        beats: u32,
        /// Accent every this many beats (0: none).
        accent_every: u8,
    },
    /// `CancelSoundcheck`.
    CancelSoundcheck,
}

/// One line of `events.jsonl`.
#[derive(Clone, Debug, PartialEq)]
pub enum TapeEvent {
    /// What the engine told the client (line types `onset`, `mix`, `clock`, `step`,
    /// `unjudged`, `backend`, `stopped`, `reopened`, `dropped`). `Pitch` is not taped.
    Engine(EngineEvent),
    /// A session command (`cmd`).
    Cmd(SessionCmdLog),
    /// The anchor of a round: output frame of chart time zero, and the chart as JSON
    /// (`anchor`). Stored as a nested object when `chart_json` parses; a chart that went
    /// in as compact text comes back as the same text.
    Anchor {
        /// Output frame of chart time zero.
        out_frame: u64,
        /// The chart, `Chart::to_json`.
        chart_json: String,
        /// The timing reference this round is judged with. `None` on tapes written before
        /// the field existed (the header's, which is the latest round's, applies).
        timing: Option<TimingRef>,
        /// The judge settings of this round. `None` as for `timing`.
        judge: Option<JudgeConfig>,
    },
    /// What the judge said (`flash`, `verdict` (provisional or final), `withdraw`, `extra`).
    Out(JudgeOut),
    /// Statistics of a finished round (`round`).
    Round(RoundStats),
    /// A take starts here (`take`).
    Take {
        /// Label of the take.
        label: String,
    },
    /// What the virtual player really did (`truth`, test bench only).
    Truth(TruthStrum),
}

fn onset_json(n: &OnsetNote) -> Json {
    let mut p = vec![("e", Json::Null)];
    match n.event {
        OnsetEvent::Heard { t, at } => {
            p[0].1 = Json::s("heard");
            p.push(("t", Json::f64(t)));
            p.push(("at", Json::f64(at)));
        }
        OnsetEvent::Strum(s) => {
            p[0].1 = Json::s("strum");
            p.push(("t", Json::f64(s.t)));
            p.push(("spread", Json::f64(s.spread)));
            p.push(("peaks", Json::u(u64::from(s.peaks))));
            p.push(("strength", Json::f32(s.strength)));
            p.push(("rise_db", Json::f32(s.rise_db)));
        }
    }
    p.push(("t_in", Json::f64(n.t_in)));
    p.push(("at_in", Json::opt_f64(n.at_in)));
    p.push(("judged", Json::Bool(n.judged)));
    p.push(("epoch", Json::u(u64::from(n.epoch))));
    Json::obj(p)
}

fn onset_from(f: &Fields) -> Result<OnsetNote, String> {
    let event = match f.str("e")? {
        "heard" => OnsetEvent::Heard {
            t: f.f64("t")?,
            at: f.f64("at")?,
        },
        "strum" => OnsetEvent::Strum(Strum {
            t: f.f64("t")?,
            spread: f.f64("spread")?,
            peaks: f.u16("peaks")?,
            strength: f.f32("strength")?,
            rise_db: f.f32("rise_db")?,
        }),
        other => return Err(format!("unknown onset '{other}'")),
    };
    Ok(OnsetNote {
        event,
        t_in: f.f64("t_in")?,
        at_in: f.opt_f64("at_in")?,
        judged: f.bool("judged")?,
        epoch: f.u32("epoch")?,
    })
}

/// The numbers of a clock snapshot as a JSON object.
pub fn clock_parts_json(p: &ClockParts) -> Json {
    Json::obj(vec![
        ("epoch", Json::u(u64::from(p.epoch))),
        ("quality", Json::s(quality_name(p.quality))),
        ("n_in", Json::f64(p.n_in)),
        ("t_in", Json::f64(p.t_in)),
        ("p_in", Json::f64(p.p_in)),
        ("n_out", Json::f64(p.n_out)),
        ("t_out", Json::f64(p.t_out)),
        ("p_out", Json::f64(p.p_out)),
        ("fs_in", Json::f64(p.fs_in)),
        ("fs_out", Json::f64(p.fs_out)),
        ("src_in", Json::s(source_name(p.src_in))),
        ("src_out", Json::s(source_name(p.src_out))),
        (
            "base_ns",
            p.base_ns.map_or(Json::Null, |b| Json::Str(b.to_string())),
        ),
    ])
}

/// Reads [`clock_parts_json`].
pub fn clock_parts_from(f: &Fields) -> Result<ClockParts, String> {
    let base_ns = match f.opt_str("base_ns")? {
        None => None,
        Some(t) => Some(t.parse::<i128>().map_err(|_| "bad 'base_ns'".to_string())?),
    };
    Ok(ClockParts {
        epoch: f.u32("epoch")?,
        quality: quality_from(f.str("quality")?)?,
        n_in: f.f64("n_in")?,
        t_in: f.f64("t_in")?,
        p_in: f.f64("p_in")?,
        n_out: f.f64("n_out")?,
        t_out: f.f64("t_out")?,
        p_out: f.f64("p_out")?,
        fs_in: f.f64("fs_in")?,
        fs_out: f.f64("fs_out")?,
        src_in: source_from(f.str("src_in")?)?,
        src_out: source_from(f.str("src_out")?)?,
        base_ns,
    })
}

fn tendency_json(t: Tendency) -> Json {
    match t {
        Tendency::NotClaimed(n) => Json::obj(vec![
            ("t", Json::s("not_claimed")),
            ("why", Json::s(noclaim_name(n))),
        ]),
        Tendency::Centered => Json::obj(vec![("t", Json::s("centered"))]),
        Tendency::Early(ms) => Json::obj(vec![("t", Json::s("early")), ("ms", Json::f64(ms))]),
        Tendency::Late(ms) => Json::obj(vec![("t", Json::s("late")), ("ms", Json::f64(ms))]),
    }
}

fn tendency_from(f: &Fields) -> Result<Tendency, String> {
    Ok(match f.str("t")? {
        "not_claimed" => Tendency::NotClaimed(noclaim_from(f.str("why")?)?),
        "centered" => Tendency::Centered,
        "early" => Tendency::Early(f.f64("ms")?),
        "late" => Tendency::Late(f.f64("ms")?),
        other => return Err(format!("unknown tendency '{other}'")),
    })
}

fn round_json(r: &RoundStats) -> Vec<(&'static str, Json)> {
    vec![
        ("events", Json::u(u64::from(r.events))),
        ("heard", Json::u(u64::from(r.heard))),
        ("not_heard", Json::u(u64::from(r.not_heard))),
        ("not_judged", Json::u(u64::from(r.not_judged))),
        ("disputed", Json::u(u64::from(r.disputed))),
        ("center", Json::u(u64::from(r.center))),
        ("in_window", Json::u(u64::from(r.in_window))),
        ("outside", Json::u(u64::from(r.outside))),
        ("timed", Json::u(u64::from(r.timed))),
        ("median_ms", Json::opt_f64(r.median_ms)),
        ("spread_ms", Json::opt_f64(r.spread_ms)),
        ("uncertainty_ms", Json::opt_f64(r.uncertainty_ms)),
        ("p90_abs_ms", Json::opt_f64(r.p90_abs_ms)),
        ("constancy_focus", Json::Bool(r.constancy_focus)),
        ("tendency", tendency_json(r.tendency)),
    ]
}

fn round_from(f: &Fields) -> Result<RoundStats, String> {
    Ok(RoundStats {
        events: f.u32("events")?,
        heard: f.u32("heard")?,
        not_heard: f.u32("not_heard")?,
        not_judged: f.u32("not_judged")?,
        disputed: f.u32("disputed")?,
        center: f.u32("center")?,
        in_window: f.u32("in_window")?,
        outside: f.u32("outside")?,
        timed: f.u32("timed")?,
        median_ms: f.opt_f64("median_ms")?,
        spread_ms: f.opt_f64("spread_ms")?,
        uncertainty_ms: f.opt_f64("uncertainty_ms")?,
        p90_abs_ms: f.opt_f64("p90_abs_ms")?,
        constancy_focus: f.bool("constancy_focus")?,
        tendency: tendency_from(&f.obj("tendency")?)?,
    })
}

fn cmd_json(c: &SessionCmdLog) -> Vec<(&'static str, Json)> {
    let name = |n: &str| ("c", Json::s(n));
    match c {
        SessionCmdLog::Soundcheck { force } => {
            vec![name("soundcheck"), ("force", Json::Bool(*force))]
        }
        SessionCmdLog::StartRound {
            lead_in_beats,
            take,
        } => vec![
            name("start_round"),
            ("lead_in_beats", Json::u(u64::from(*lead_in_beats))),
            ("take", take.as_deref().map_or(Json::Null, Json::s)),
        ],
        SessionCmdLog::StopRound => vec![name("stop_round")],
        SessionCmdLog::Dispute => vec![name("dispute")],
        SessionCmdLog::SetLevel(l) => vec![name("set_level"), ("level", Json::s(level_name(*l)))],
        SessionCmdLog::Record(on) => vec![name("record"), ("on", Json::Bool(*on))],
        SessionCmdLog::Rewire => vec![name("rewire")],
        SessionCmdLog::Shutdown => vec![name("shutdown")],
        SessionCmdLog::Click {
            bpm,
            beats,
            accent_every,
        } => vec![
            name("click"),
            ("bpm", Json::f64(*bpm)),
            ("beats", Json::u(u64::from(*beats))),
            ("accent_every", Json::u(u64::from(*accent_every))),
        ],
        SessionCmdLog::CancelSoundcheck => vec![name("cancel_soundcheck")],
    }
}

fn cmd_from(f: &Fields) -> Result<SessionCmdLog, String> {
    Ok(match f.str("c")? {
        "soundcheck" => SessionCmdLog::Soundcheck {
            force: f.bool("force")?,
        },
        "start_round" => SessionCmdLog::StartRound {
            lead_in_beats: f.u32("lead_in_beats")?,
            take: f.opt_str("take")?.map(str::to_owned),
        },
        "stop_round" => SessionCmdLog::StopRound,
        "dispute" => SessionCmdLog::Dispute,
        "set_level" => SessionCmdLog::SetLevel(level_from(f.str("level")?)?),
        "record" => SessionCmdLog::Record(f.bool("on")?),
        "rewire" => SessionCmdLog::Rewire,
        "shutdown" => SessionCmdLog::Shutdown,
        "click" => SessionCmdLog::Click {
            bpm: f.f64("bpm")?,
            beats: f.u32("beats")?,
            accent_every: f.u8("accent_every")?,
        },
        "cancel_soundcheck" => SessionCmdLog::CancelSoundcheck,
        other => return Err(format!("unknown command '{other}'")),
    })
}

/// The line of `ev` as a JSON object: `{"seq":…,"k":…,…}`. `None` for events that are not
/// taped (tuner frames).
pub fn encode_event(seq: u64, ev: &TapeEvent) -> Option<Json> {
    let mut p: Vec<(&str, Json)> = vec![("seq", Json::u(seq))];
    let k = |p: &mut Vec<(&str, Json)>, name: &str| p.push(("k", Json::s(name)));
    match ev {
        TapeEvent::Engine(e) => match e {
            EngineEvent::Onset(n) => {
                k(&mut p, "onset");
                if let Json::Obj(v) = onset_json(n) {
                    return Some(merge(p, v));
                }
            }
            EngineEvent::Pitch(_) => return None,
            EngineEvent::Mix(m) => {
                k(&mut p, "mix");
                match *m {
                    MixEvent::Started { tag, at } => {
                        p.push(("e", Json::s("started")));
                        p.push(("tag", Json::u(u64::from(tag))));
                        p.push(("at", Json::u(at)));
                    }
                    MixEvent::Beat { tag, index, at } => {
                        p.push(("e", Json::s("beat")));
                        p.push(("tag", Json::u(u64::from(tag))));
                        p.push(("index", Json::u(u64::from(index))));
                        p.push(("at", Json::u(at)));
                    }
                    MixEvent::Late { tag, by } => {
                        p.push(("e", Json::s("late")));
                        p.push(("tag", Json::u(u64::from(tag))));
                        p.push(("by", Json::u(u64::from(by))));
                    }
                    MixEvent::Rejected { tag } => {
                        p.push(("e", Json::s("rejected")));
                        p.push(("tag", Json::u(u64::from(tag))));
                    }
                }
            }
            EngineEvent::Clock(s) => {
                k(&mut p, "clock");
                if let Json::Obj(v) = clock_parts_json(&s.parts()) {
                    return Some(merge(p, v));
                }
            }
            EngineEvent::Step(s) => {
                k(&mut p, "step");
                p.push(("side", Json::s(side_name(s.side))));
                p.push(("epoch", Json::u(u64::from(s.epoch))));
                p.push(("frame", Json::u(s.frame)));
                p.push(("step_s", Json::f64(s.step_s)));
                p.push(("frames", Json::f64(s.frames)));
                p.push(("cause", Json::s(cause_name(s.cause))));
            }
            EngineEvent::Unjudged {
                epoch,
                from_in,
                to_in,
            } => {
                k(&mut p, "unjudged");
                p.push(("epoch", Json::u(u64::from(*epoch))));
                p.push(("from_in", Json::f64(*from_in)));
                p.push(("to_in", Json::f64(*to_in)));
            }
            EngineEvent::Backend(b) => {
                k(&mut p, "backend");
                p.push(("e", Json::s(backend_name(*b))));
            }
            EngineEvent::Stopped(b) => {
                k(&mut p, "stopped");
                p.push(("e", Json::s(backend_name(*b))));
            }
            EngineEvent::Reopened { epoch } => {
                k(&mut p, "reopened");
                p.push(("epoch", Json::u(u64::from(*epoch))));
            }
            EngineEvent::Dropped { ring, n } => {
                k(&mut p, "dropped");
                p.push(("ring", Json::s(ring_name(*ring))));
                p.push(("n", Json::u(u64::from(*n))));
            }
        },
        TapeEvent::Cmd(c) => {
            k(&mut p, "cmd");
            return Some(merge(
                p,
                cmd_json(c)
                    .into_iter()
                    .map(|(a, b)| (a.to_string(), b))
                    .collect(),
            ));
        }
        TapeEvent::Anchor {
            out_frame,
            chart_json,
            timing,
            judge,
        } => {
            k(&mut p, "anchor");
            p.push(("out_frame", Json::u(*out_frame)));
            p.push((
                "chart",
                Json::parse(chart_json).unwrap_or_else(|_| Json::Str(chart_json.clone())),
            ));
            // Absent (not `null`) when unknown, so an old line reads back as it was.
            if let Some(t) = timing {
                p.push(("timing", timing_ref_json(t)));
            }
            if let Some(j) = judge {
                p.push(("judge", judge_config_json(j)));
            }
        }
        TapeEvent::Out(o) => match *o {
            JudgeOut::Flash { id, dt_ms } => {
                k(&mut p, "flash");
                p.push(("id", id.map_or(Json::Null, |i| Json::u(u64::from(i)))));
                p.push(("dt_ms", dt_ms.map_or(Json::Null, Json::f32)));
            }
            JudgeOut::Verdict(v) => {
                k(&mut p, "verdict");
                p.push(("id", Json::u(u64::from(v.id))));
                p.push(("attack", Json::Str(attack_name(v.attack))));
                p.push(("timing", Json::Str(timing_name(v.timing))));
                p.push(("dt_ms", v.dt_ms.map_or(Json::Null, Json::f32)));
                p.push(("dir", Json::Str(dir_name(v.dir))));
                p.push(("onset_in", Json::opt_f64(v.onset_in)));
                p.push(("pos_in_bar", Json::u(u64::from(v.pos_in_bar))));
                p.push(("fin", Json::Bool(v.fin)));
            }
            JudgeOut::Withdrawn { id, why } => {
                k(&mut p, "withdraw");
                p.push(("id", Json::u(u64::from(id))));
                p.push(("why", Json::s(why_name(why))));
            }
            JudgeOut::Extra { t_in } => {
                k(&mut p, "extra");
                p.push(("t_in", Json::f64(t_in)));
            }
        },
        TapeEvent::Round(r) => {
            k(&mut p, "round");
            p.extend(round_json(r));
        }
        TapeEvent::Take { label } => {
            k(&mut p, "take");
            p.push(("label", Json::s(label)));
        }
        TapeEvent::Truth(t) => {
            k(&mut p, "truth");
            p.push(("id", t.id.map_or(Json::Null, |i| Json::u(u64::from(i)))));
            p.push(("kind", Json::s(kind_name(t.kind))));
            p.push(("out_frame", Json::u(t.out_frame)));
            p.push(("played", Json::Bool(t.played)));
            p.push(("extra", Json::Bool(t.extra)));
            p.push(("muted", Json::Bool(t.muted)));
            p.push(("g_in", Json::f64(t.g_in)));
            p.push(("arrival_in", Json::f64(t.arrival_in)));
            p.push(("onset_in", Json::opt_f64(t.onset_in)));
            p.push(("dt_ms", Json::opt_f64(t.dt_ms)));
        }
    }
    Some(Json::obj(p))
}

fn merge(head: Vec<(&str, Json)>, tail: Vec<(String, Json)>) -> Json {
    let mut v: Vec<(String, Json)> = head.into_iter().map(|(k, j)| (k.to_string(), j)).collect();
    v.extend(tail);
    Json::Obj(v)
}

/// What a line of `events.jsonl` turned out to be.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    /// A known line.
    Event(u64, TapeEvent),
    /// A well-formed line of a type this reader does not know.
    Unknown,
    /// Not JSON, not an object, or a known type with a missing or wrong field.
    Bad,
}

/// Reads one line of `events.jsonl`.
pub fn decode_line(line: &str) -> Decoded {
    let Ok(v) = Json::parse(line) else {
        return Decoded::Bad;
    };
    let Ok(f) = Fields::new(&v) else {
        return Decoded::Bad;
    };
    let (Ok(seq), Ok(k)) = (f.u64("seq"), f.str("k")) else {
        return Decoded::Bad;
    };
    match decode_kind(k, &f) {
        Ok(Some(ev)) => Decoded::Event(seq, ev),
        Ok(None) => Decoded::Unknown,
        Err(_) => Decoded::Bad,
    }
}

fn decode_kind(k: &str, f: &Fields) -> Result<Option<TapeEvent>, String> {
    use EngineEvent as E;
    let engine = |e| Ok(Some(TapeEvent::Engine(e)));
    match k {
        "onset" => engine(E::Onset(onset_from(f)?)),
        "mix" => engine(E::Mix(match f.str("e")? {
            "started" => MixEvent::Started {
                tag: f.u32("tag")?,
                at: f.u64("at")?,
            },
            "beat" => MixEvent::Beat {
                tag: f.u32("tag")?,
                index: f.u32("index")?,
                at: f.u64("at")?,
            },
            "late" => MixEvent::Late {
                tag: f.u32("tag")?,
                by: f.u32("by")?,
            },
            "rejected" => MixEvent::Rejected { tag: f.u32("tag")? },
            other => return Err(format!("unknown mix event '{other}'")),
        })),
        "clock" => engine(E::Clock(guitarness_audio::ClockSnapshot::from_parts(
            clock_parts_from(f)?,
        ))),
        "step" => engine(E::Step(ClockStep {
            side: side_from(f.str("side")?)?,
            epoch: f.u32("epoch")?,
            frame: f.u64("frame")?,
            step_s: f.f64("step_s")?,
            frames: f.f64("frames")?,
            cause: cause_from(f.str("cause")?)?,
        })),
        "unjudged" => engine(E::Unjudged {
            epoch: f.u32("epoch")?,
            from_in: f.f64("from_in")?,
            to_in: f.f64("to_in")?,
        }),
        "backend" => engine(E::Backend(backend_from(f.str("e")?)?)),
        "stopped" => engine(E::Stopped(backend_from(f.str("e")?)?)),
        "reopened" => engine(E::Reopened {
            epoch: f.u32("epoch")?,
        }),
        "dropped" => engine(E::Dropped {
            ring: ring_from(f.str("ring")?)?,
            n: f.u32("n")?,
        }),
        "cmd" => Ok(Some(TapeEvent::Cmd(cmd_from(f)?))),
        "anchor" => Ok(Some(TapeEvent::Anchor {
            out_frame: f.u64("out_frame")?,
            chart_json: match f.raw("chart")? {
                Json::Str(s) => s.clone(),
                other => other.to_text(),
            },
            timing: match f.opt_member("timing") {
                Some(Json::Null) | None => None,
                Some(v) => Some(timing_ref_from(&Fields::new(v)?)?),
            },
            judge: match f.opt_member("judge") {
                Some(Json::Null) | None => None,
                Some(v) => Some(judge_config_from(&Fields::new(v)?)?),
            },
        })),
        "flash" => Ok(Some(TapeEvent::Out(JudgeOut::Flash {
            id: f
                .opt_u64("id")?
                .map(u32::try_from)
                .transpose()
                .map_err(|_| "bad id")?,
            dt_ms: f.opt_f32("dt_ms")?,
        }))),
        "verdict" => Ok(Some(TapeEvent::Out(JudgeOut::Verdict(PulseVerdict {
            id: f.u32("id")?,
            attack: attack_from(f.str("attack")?)?,
            timing: timing_from(f.str("timing")?)?,
            dt_ms: f.opt_f32("dt_ms")?,
            dir: dir_from(f.str("dir")?)?,
            onset_in: f.opt_f64("onset_in")?,
            pos_in_bar: f.u8("pos_in_bar")?,
            fin: f.bool("fin")?,
        })))),
        "withdraw" => Ok(Some(TapeEvent::Out(JudgeOut::Withdrawn {
            id: f.u32("id")?,
            why: why_from(f.str("why")?)?,
        }))),
        "extra" => Ok(Some(TapeEvent::Out(JudgeOut::Extra {
            t_in: f.f64("t_in")?,
        }))),
        "round" => Ok(Some(TapeEvent::Round(round_from(f)?))),
        "take" => Ok(Some(TapeEvent::Take {
            label: f.str("label")?.to_owned(),
        })),
        "truth" => Ok(Some(TapeEvent::Truth(TruthStrum {
            id: f
                .opt_u64("id")?
                .map(u32::try_from)
                .transpose()
                .map_err(|_| "bad id")?,
            kind: kind_from(f.str("kind")?)?,
            out_frame: f.u64("out_frame")?,
            played: f.bool("played")?,
            extra: f.bool("extra")?,
            muted: f.bool("muted")?,
            g_in: f.f64("g_in")?,
            arrival_in: f.f64("arrival_in")?,
            onset_in: f.opt_f64("onset_in")?,
            dt_ms: f.opt_f64("dt_ms")?,
        }))),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------------------
// Header, runs
// ---------------------------------------------------------------------------------------

/// A take: events `seq_first..=seq_last` belong to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakeSpan {
    /// Label of the take.
    pub label: String,
    /// `seq` of its `take` line.
    pub seq_first: u64,
    /// `seq` of its last line.
    pub seq_last: u64,
}

/// Counts kept in `tape.json`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TapeCounts {
    /// Lines in `events.jsonl`.
    pub events: u64,
    /// Samples in `mic.wav`.
    pub mic_frames: u64,
    /// Of those, zero-filled because the raw ring lost the block.
    pub lost_frames: u64,
    /// Raw blocks the ring dropped (`RawOutput::dropped_blocks`).
    pub dropped_raw_blocks: u64,
    /// Tuner frames that were not taped.
    pub skipped: u64,
}

/// `tape.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct TapeHeader {
    /// App version (`CARGO_PKG_VERSION` of this crate).
    pub app_version: String,
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// UTC creation time, `YYYY-MM-DDTHH:MM:SSZ`.
    pub created: String,
    /// Input rate, Hz.
    pub fs_in: u32,
    /// Output rate, Hz.
    pub fs_out: u32,
    /// Input channels.
    pub ch_in: u16,
    /// Output channels.
    pub ch_out: u16,
    /// Input device id.
    pub in_id: String,
    /// Output device id.
    pub out_id: String,
    /// `StreamInfo::origin_offset_ns`.
    pub origin_offset_ns: i64,
    /// Stamps that feed the input line.
    pub stamp_in: StampSource,
    /// Stamps that feed the output line.
    pub stamp_out: StampSource,
    /// The calibration profile in force (`Profile::to_json`), if any.
    pub profile: Option<Json>,
    /// The timing reference the round was judged with.
    pub timing: TimingRef,
    /// The judge settings.
    pub judge: JudgeConfig,
    /// The gate table (`GateTable::to_json`), if one was in force.
    pub gates: Option<Json>,
    /// Offset between the video and the audio, when a screen recording goes with the tape.
    pub video_offset_ms: f64,
    /// Takes, from `take` lines (rebuilt by the reader when the tape was not closed).
    pub takes: Vec<TakeSpan>,
    /// Counts.
    pub counts: TapeCounts,
    /// The tape has `mic.wav` and `mic.runs.json` (set by the writer; a closed tape that
    /// says so and lacks them is damaged).
    pub has_audio: bool,
    /// The player contested a verdict: `prune` never deletes the tape.
    pub disputed: bool,
    /// The tape was closed by `finish` and nothing was lost. Replay gates only use tapes
    /// where this is true.
    pub complete: bool,
}

impl TapeHeader {
    /// A header for these rates: current time, this app and system, no profile, timing
    /// `None`, apprentice level, device stamps, `complete = false`.
    pub fn new(fs_in: u32, fs_out: u32) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Self {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            created: utc_iso(now),
            fs_in,
            fs_out,
            ch_in: 1,
            ch_out: 2,
            in_id: String::new(),
            out_id: String::new(),
            origin_offset_ns: 0,
            stamp_in: StampSource::Device,
            stamp_out: StampSource::Device,
            profile: None,
            timing: TimingRef::none(),
            judge: JudgeConfig::new(f64::from(fs_in), Level::Apprentice),
            gates: None,
            video_offset_ms: 0.0,
            takes: Vec::new(),
            counts: TapeCounts::default(),
            has_audio: false,
            disputed: false,
            complete: false,
        }
    }

    /// Stores `p` as the profile in force.
    pub fn set_profile(&mut self, p: &Profile) {
        self.profile = Some(Json::from_serde(&p.to_json()));
    }

    /// The stored profile, read back (`None` if absent or not a valid profile).
    pub fn profile(&self) -> Option<Profile> {
        Profile::from_json(&self.profile.as_ref()?.to_serde())
    }

    /// Stores `g` as the gate table in force.
    pub fn set_gates(&mut self, g: &GateTable) {
        self.gates = Json::parse(&g.to_json()).ok();
    }

    /// The stored gate table (`None` if absent or invalid).
    pub fn gates(&self) -> Option<GateTable> {
        GateTable::from_json(&self.gates.as_ref()?.to_text()).ok()
    }
}

fn timing_ref_json(t: &TimingRef) -> Json {
    match t.basis {
        TimingBasis::Loopback { l_rt_ms, mad_ms } => Json::obj(vec![
            ("basis", Json::s("loopback")),
            ("l_rt_ms", Json::f64(l_rt_ms)),
            ("mad_ms", Json::f64(mad_ms)),
        ]),
        TimingBasis::Estimate {
            offset_ms,
            mad_ms,
            n,
        } => Json::obj(vec![
            ("basis", Json::s("estimate")),
            ("offset_ms", Json::f64(offset_ms)),
            ("mad_ms", Json::f64(mad_ms)),
            ("n", Json::u(u64::from(n))),
        ]),
        TimingBasis::None => Json::obj(vec![("basis", Json::s("none"))]),
    }
}

fn timing_ref_from(f: &Fields) -> Result<TimingRef, String> {
    Ok(match f.str("basis")? {
        "loopback" => TimingRef::loopback(f.f64("l_rt_ms")?, f.f64("mad_ms")?),
        "estimate" => TimingRef::estimate(f.f64("offset_ms")?, f.f64("mad_ms")?, f.u32("n")?),
        "none" => TimingRef::none(),
        other => return Err(format!("unknown basis '{other}'")),
    })
}

fn judge_config_json(j: &JudgeConfig) -> Json {
    Json::obj(vec![
        ("fs_in", Json::f64(j.fs_in)),
        ("level", Json::s(level_name(j.level))),
        ("center_ms", Json::f64(j.center_ms)),
        ("pair_max_ms", Json::f64(j.pair_max_ms)),
        ("close_margin_ms", Json::f64(j.close_margin_ms)),
    ])
}

fn judge_config_from(f: &Fields) -> Result<JudgeConfig, String> {
    Ok(JudgeConfig {
        fs_in: f.f64("fs_in")?,
        level: level_from(f.str("level")?)?,
        center_ms: f.f64("center_ms")?,
        pair_max_ms: f.f64("pair_max_ms")?,
        close_margin_ms: f.f64("close_margin_ms")?,
    })
}

/// `tape.json` as text.
pub fn header_to_text(h: &TapeHeader) -> String {
    let takes = h
        .takes
        .iter()
        .map(|t| {
            Json::obj(vec![
                ("label", Json::s(&t.label)),
                ("seq_first", Json::u(t.seq_first)),
                ("seq_last", Json::u(t.seq_last)),
            ])
        })
        .collect();
    let c = &h.counts;
    Json::obj(vec![
        ("v", Json::u(TAPE_VERSION)),
        ("app_version", Json::s(&h.app_version)),
        ("os", Json::s(&h.os)),
        ("arch", Json::s(&h.arch)),
        ("created", Json::s(&h.created)),
        ("fs_in", Json::u(u64::from(h.fs_in))),
        ("fs_out", Json::u(u64::from(h.fs_out))),
        ("ch_in", Json::u(u64::from(h.ch_in))),
        ("ch_out", Json::u(u64::from(h.ch_out))),
        ("in_id", Json::s(&h.in_id)),
        ("out_id", Json::s(&h.out_id)),
        ("origin_offset_ns", Json::i(h.origin_offset_ns)),
        ("stamp_in", Json::s(source_name(h.stamp_in))),
        ("stamp_out", Json::s(source_name(h.stamp_out))),
        ("profile", h.profile.clone().unwrap_or(Json::Null)),
        ("timing", timing_ref_json(&h.timing)),
        ("judge", judge_config_json(&h.judge)),
        ("gates", h.gates.clone().unwrap_or(Json::Null)),
        ("video_offset_ms", Json::f64(h.video_offset_ms)),
        ("takes", Json::Arr(takes)),
        (
            "counts",
            Json::obj(vec![
                ("events", Json::u(c.events)),
                ("mic_frames", Json::u(c.mic_frames)),
                ("lost_frames", Json::u(c.lost_frames)),
                ("dropped_raw_blocks", Json::u(c.dropped_raw_blocks)),
                ("skipped", Json::u(c.skipped)),
            ]),
        ),
        ("has_audio", Json::Bool(h.has_audio)),
        ("disputed", Json::Bool(h.disputed)),
        ("complete", Json::Bool(h.complete)),
    ])
    .to_text_pretty()
}

/// Why a tape could not be read.
#[derive(Debug)]
pub enum TapeError {
    /// A file operation failed.
    Io(io::Error),
    /// `v` is not [`TAPE_VERSION`] (`None`: missing).
    Version(Option<u64>),
    /// A file is not what the format says.
    Format(String),
    /// The tape has no `mic.wav`.
    NoAudio,
    /// A window that cannot be returned.
    Range(String),
}

impl std::fmt::Display for TapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "tape i/o: {e}"),
            Self::Version(Some(v)) => write!(f, "tape version {v} is not {TAPE_VERSION}"),
            Self::Version(None) => write!(f, "tape has no version"),
            Self::Format(m) => write!(f, "tape format: {m}"),
            Self::NoAudio => write!(f, "tape has no audio"),
            Self::Range(m) => write!(f, "tape window: {m}"),
        }
    }
}

impl std::error::Error for TapeError {}

impl From<io::Error> for TapeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Reads `tape.json` text.
pub fn header_from_text(text: &str) -> Result<TapeHeader, TapeError> {
    let v = Json::parse(text).map_err(TapeError::Format)?;
    let f = Fields::new(&v).map_err(TapeError::Format)?;
    let version = f.opt_u64("v").ok().flatten();
    if version != Some(TAPE_VERSION) {
        return Err(TapeError::Version(version));
    }
    header_fields(&f).map_err(TapeError::Format)
}

fn header_fields(f: &Fields) -> Result<TapeHeader, String> {
    let opt_json = |k: &str| -> Result<Option<Json>, String> {
        Ok(match f.raw(k)? {
            Json::Null => None,
            other => Some(other.clone()),
        })
    };
    let judge = f.obj("judge")?;
    let counts = f.obj("counts")?;
    let mut takes = Vec::new();
    for t in f.arr("takes")? {
        let t = Fields::new(t)?;
        takes.push(TakeSpan {
            label: t.str("label")?.to_owned(),
            seq_first: t.u64("seq_first")?,
            seq_last: t.u64("seq_last")?,
        });
    }
    Ok(TapeHeader {
        app_version: f.str("app_version")?.to_owned(),
        os: f.str("os")?.to_owned(),
        arch: f.str("arch")?.to_owned(),
        created: f.str("created")?.to_owned(),
        fs_in: f.u32("fs_in")?,
        fs_out: f.u32("fs_out")?,
        ch_in: f.u16("ch_in")?,
        ch_out: f.u16("ch_out")?,
        in_id: f.str("in_id")?.to_owned(),
        out_id: f.str("out_id")?.to_owned(),
        origin_offset_ns: f.i64("origin_offset_ns")?,
        stamp_in: source_from(f.str("stamp_in")?)?,
        stamp_out: source_from(f.str("stamp_out")?)?,
        profile: opt_json("profile")?,
        timing: timing_ref_from(&f.obj("timing")?)?,
        judge: judge_config_from(&judge)?,
        gates: opt_json("gates")?,
        video_offset_ms: f.f64("video_offset_ms")?,
        takes,
        counts: TapeCounts {
            events: counts.u64("events")?,
            mic_frames: counts.u64("mic_frames")?,
            lost_frames: counts.u64("lost_frames")?,
            dropped_raw_blocks: counts.u64("dropped_raw_blocks")?,
            skipped: counts.u64("skipped")?,
        },
        has_audio: f.bool("has_audio")?,
        disputed: f.bool("disputed")?,
        complete: f.bool("complete")?,
    })
}

/// A stretch of `mic.wav` with one constant offset between input frame and detector time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MicRun {
    /// Detector time of the first sample of the run.
    pub det_first: u64,
    /// Input frame (`InFrame`) of the first sample of the run.
    pub in_first: u64,
    /// Samples in the run.
    pub frames: u64,
    /// The raw ring lost these samples: the file holds zeros here, and `in_first` assumes
    /// the offset that follows.
    pub lost: bool,
}

/// `mic.runs.json` as text.
pub fn runs_to_text(fs_in: u32, runs: &[MicRun]) -> String {
    let list = runs
        .iter()
        .map(|r| {
            Json::obj(vec![
                ("det_first", Json::u(r.det_first)),
                ("in_first", Json::u(r.in_first)),
                ("frames", Json::u(r.frames)),
                ("lost", Json::Bool(r.lost)),
            ])
        })
        .collect();
    Json::obj(vec![
        ("v", Json::u(TAPE_VERSION)),
        ("fs_in", Json::u(u64::from(fs_in))),
        ("runs", Json::Arr(list)),
    ])
    .to_text_pretty()
}

/// Reads `mic.runs.json` text: the rate and the runs.
pub fn runs_from_text(text: &str) -> Result<(u32, Vec<MicRun>), TapeError> {
    let v = Json::parse(text).map_err(TapeError::Format)?;
    let f = Fields::new(&v).map_err(TapeError::Format)?;
    let version = f.opt_u64("v").ok().flatten();
    if version != Some(TAPE_VERSION) {
        return Err(TapeError::Version(version));
    }
    let one = || -> Result<(u32, Vec<MicRun>), String> {
        let mut runs = Vec::new();
        for r in f.arr("runs")? {
            let r = Fields::new(r)?;
            runs.push(MicRun {
                det_first: r.u64("det_first")?,
                in_first: r.u64("in_first")?,
                frames: r.u64("frames")?,
                lost: r.bool("lost")?,
            });
        }
        Ok((f.u32("fs_in")?, runs))
    };
    one().map_err(TapeError::Format)
}

// ---------------------------------------------------------------------------------------
// Names and time
// ---------------------------------------------------------------------------------------

fn civil(unix_secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (unix_secs / 86_400) as i64;
    let rem = (unix_secs % 86_400) as u32;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC).
pub fn utc_iso(unix_secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(unix_secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Folder name of a tape: `AAAAMMDD-HHMMSS-<4 hex>` (UTC), plan §3.
pub fn tape_folder_name(unix_secs: u64, suffix: u16) -> String {
    let (y, mo, d, h, mi, s) = civil(unix_secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}-{suffix:04x}")
}

/// Creates a new, empty tape folder named as the plan says inside `root`, the folder that
/// holds the tapes (`<dados>/tapes`, created if missing). The 4 hex digits come from the
/// clock and a counter; a name that exists is skipped.
pub fn new_tape_dir(root: &std::path::Path) -> io::Result<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    std::fs::create_dir_all(root)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    for _ in 0..64 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mix = now.subsec_nanos() ^ n.wrapping_mul(0x9E37_79B1) ^ std::process::id();
        let suffix = (mix ^ (mix >> 16)) as u16;
        let dir = root.join(tape_folder_name(now.as_secs(), suffix));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free tape folder name",
    ))
}

/// The line text of an event with `seq` (without the newline), `None` if not taped.
pub fn event_line(seq: u64, ev: &TapeEvent) -> Option<String> {
    encode_event(seq, ev).map(|j| j.to_text())
}
