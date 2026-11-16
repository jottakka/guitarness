//! Chart v1 (`chart.json`, DESIGN 4.3 plus addendum A2): tempo map, time
//! signatures, voicings and events. Pure data; no Bevy, no audio, no files.
//!
//! JSON is read and written by hand over `serde_json::Value`. Unknown fields
//! are ignored on read (and therefore dropped on write), except `lyrics`,
//! which is kept as opaque values.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value, json};

/// Standard guitar tuning, string 0 = 6th (low E), MIDI note numbers.
pub const STANDARD_TUNING: [i32; 6] = [40, 45, 50, 55, 59, 64];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    Down,
    Up,
}

impl Dir {
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::Down => "D",
            Dir::Up => "U",
        }
    }
}

/// Stroke style of a `strum` event (`style` in the JSON; absent = `Full`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Style {
    Full,
    Accent,
    Roll,
    /// Palm mute (`"pm"`).
    Pm,
}

impl Style {
    fn as_str(self) -> &'static str {
        match self {
            Style::Full => "full",
            Style::Accent => "accent",
            Style::Roll => "roll",
            Style::Pm => "pm",
        }
    }
}

/// Strings a stroke must reach, inclusive, 0 = 6th string, 0..=5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub lo: u8,
    pub hi: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// Pitched stroke on a chord. `span: None` means the default for `dir`
    /// (down: 0..=5, up: 2..=5). `roll_ms` is the total spread of a roll,
    /// first to last string; `None` means 90 ms between strings.
    Strum {
        dir: Dir,
        span: Option<Span>,
        style: Style,
        roll_ms: Option<u32>,
    },
    /// Chuck: all strings damped, attack required, no pitch.
    Mute {
        dir: Dir,
    },
    /// Air stroke, never judged.
    Ghost {
        dir: Dir,
    },
    /// Single note. `finger` is a right-hand letter: p, i, m or a.
    Note {
        string: u8,
        fret: u8,
        finger: Option<char>,
    },
    Rest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub tick: u32,
    pub dur: u32,
    pub kind: EventKind,
    /// Voicing name. Required for `Strum`; optional elsewhere (notes use it as
    /// the interval root, other events inherit the previous chord).
    pub chord: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TempoPoint {
    pub tick: u32,
    pub bpm: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSig {
    pub tick: u32,
    pub num: u8,
    pub den: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Voicing {
    /// Fret per string, -1 = not played; index 0 = 6th string.
    pub frets: [i8; 6],
    /// Per-string requirement letters `M m X x` (DESIGN 4.3), 6 chars.
    pub req: Option<String>,
    /// Fretting fingers, 0 = open or unplayed.
    pub fingers: [u8; 6],
    /// Root pitch class 0..=11. When absent it is parsed from the chord name.
    pub root: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub grid: u32,
    pub cells: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub id: String,
    pub name: String,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tier {
    pub tier: u32,
    pub name: String,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub v: u32,
    pub id: String,
    pub title: String,
    pub origin: String,
    pub tuning: [i32; 6],
    pub capo: u8,
    pub tpq: u32,
    pub tempo_map: Vec<TempoPoint>,
    pub time_sigs: Vec<TimeSig>,
    pub audio: Option<String>,
    pub voicings: BTreeMap<String, Voicing>,
    pub patterns: BTreeMap<String, Pattern>,
    pub sections: Vec<Section>,
    pub tiers: Vec<Tier>,
    pub lyrics: Vec<Value>,
}

/// Position of an event inside a chart, for error messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    pub tier: usize,
    pub index: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChartError {
    /// Not valid JSON (message from the parser).
    Json(String),
    /// The document or a part that must be an object is not.
    NotObject(String),
    MissingField(String),
    WrongType(String),
    BadVersion(i64),
    EmptyTempoMap,
    /// First tempo point not at tick 0, or points not strictly increasing.
    BadTempoMap(&'static str),
    BadTempo(f64),
    EmptyTimeSigs,
    BadTimeSigs(&'static str),
    BadTpq,
    TicksOutOfOrder(Loc),
    ZeroDuration(Loc),
    /// `span` has a string outside 0..=5.
    SpanOutOfRange(Loc),
    /// `span` has lo > hi.
    SpanReversed(Loc),
    UnknownKind(Loc, String),
    UnknownChord(Loc, String),
    BadValue(Loc, &'static str),
    BadVoicing(String, &'static str),
}

impl fmt::Display for ChartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ChartError {}

impl Chart {
    /// Empty 4/4 chart at the given tempo, tpq 480, standard tuning.
    pub fn new(id: &str, bpm: f64) -> Chart {
        Chart {
            v: 1,
            id: id.to_string(),
            title: String::new(),
            origin: "own".to_string(),
            tuning: STANDARD_TUNING,
            capo: 0,
            tpq: 480,
            tempo_map: vec![TempoPoint { tick: 0, bpm }],
            time_sigs: vec![TimeSig {
                tick: 0,
                num: 4,
                den: 4,
            }],
            audio: None,
            voicings: BTreeMap::new(),
            patterns: BTreeMap::new(),
            sections: Vec::new(),
            tiers: vec![Tier {
                tier: 2,
                name: "Batida".to_string(),
                events: Vec::new(),
            }],
            lyrics: Vec::new(),
        }
    }

    /// Events of the first tier (the one that is played and judged).
    pub fn events(&self) -> &[Event] {
        self.tiers.first().map_or(&[], |t| t.events.as_slice())
    }

    /// Root pitch class of a voicing name (explicit `root`, else from the name).
    pub fn root_pc(&self, chord: &str) -> u8 {
        self.voicings
            .get(chord)
            .and_then(|v| v.root)
            .or_else(|| root_pc_of_name(chord))
            .unwrap_or(0)
    }

    // ---- time -----------------------------------------------------------

    /// Seconds from tick 0 to `t`, following the tempo map.
    pub fn seconds_of_tick(&self, t: u32) -> f64 {
        let tpq = f64::from(self.tpq.max(1));
        let pts = &self.tempo_map;
        if pts.is_empty() {
            return f64::from(t) * 60.0 / (120.0 * tpq);
        }
        let mut sec = 0.0;
        for (i, p) in pts.iter().enumerate() {
            let end = pts.get(i + 1).map_or(u32::MAX, |n| n.tick);
            if t <= p.tick {
                break;
            }
            let span = f64::from(t.min(end) - p.tick);
            sec += span * 60.0 / (p.bpm * tpq);
            if t <= end {
                break;
            }
        }
        sec
    }

    /// Inverse of [`seconds_of_tick`](Self::seconds_of_tick), rounded to the
    /// nearest tick (negative input gives 0).
    pub fn tick_of_seconds(&self, s: f64) -> u32 {
        if s <= 0.0 {
            return 0;
        }
        let tpq = f64::from(self.tpq.max(1));
        let pts = &self.tempo_map;
        if pts.is_empty() {
            return (s * 120.0 * tpq / 60.0).round() as u32;
        }
        let mut left = s;
        for (i, p) in pts.iter().enumerate() {
            let sec_per_tick = 60.0 / (p.bpm * tpq);
            match pts.get(i + 1) {
                Some(n) => {
                    let seg = f64::from(n.tick - p.tick) * sec_per_tick;
                    if left < seg {
                        return p.tick + (left / sec_per_tick).round() as u32;
                    }
                    left -= seg;
                }
                None => return p.tick + (left / sec_per_tick).round() as u32,
            }
        }
        0
    }

    /// Tempo in force at `tick`.
    pub fn bpm_at(&self, tick: u32) -> f64 {
        self.tempo_map
            .iter()
            .rev()
            .find(|p| p.tick <= tick)
            .or(self.tempo_map.first())
            .map_or(120.0, |p| p.bpm)
    }

    /// Time signature in force at `tick` (4/4 when none).
    pub fn sig_at(&self, tick: u32) -> TimeSig {
        self.time_sigs
            .iter()
            .rev()
            .find(|s| s.tick <= tick)
            .or(self.time_sigs.first())
            .copied()
            .unwrap_or(TimeSig {
                tick: 0,
                num: 4,
                den: 4,
            })
    }

    /// Ticks in one bar of `sig`.
    pub fn bar_ticks(&self, sig: TimeSig) -> u32 {
        (self.tpq * 4 * u32::from(sig.num) / u32::from(sig.den.max(1))).max(1)
    }

    /// Bar index (0-based) and tick offset inside the bar. A signature change
    /// that does not fall on a bar line counts the partial bar as whole.
    pub fn bar_pos(&self, tick: u32) -> (u32, u32) {
        let mut bars = 0u32;
        let mut sig = self.sig_at(0);
        let mut start = 0u32;
        for s in self.time_sigs.iter().skip(1) {
            if s.tick > tick {
                break;
            }
            let bt = self.bar_ticks(sig);
            bars += (s.tick - start).div_ceil(bt);
            start = s.tick;
            sig = *s;
        }
        let bt = self.bar_ticks(sig);
        (bars + (tick - start) / bt, (tick - start) % bt)
    }

    /// Position inside the bar in eighth-note slots (4/4: 0..=7), saturating
    /// at 255.
    pub fn eighth_in_bar(&self, tick: u32) -> u8 {
        let (_, off) = self.bar_pos(tick);
        (off / (self.tpq / 2).max(1)).min(255) as u8
    }

    // ---- validation -----------------------------------------------------

    pub fn validate(&self) -> Result<(), ChartError> {
        if self.v != 1 {
            return Err(ChartError::BadVersion(i64::from(self.v)));
        }
        if self.tpq == 0 {
            return Err(ChartError::BadTpq);
        }
        if self.tempo_map.is_empty() {
            return Err(ChartError::EmptyTempoMap);
        }
        if self.tempo_map[0].tick != 0 {
            return Err(ChartError::BadTempoMap("first point must be at tick 0"));
        }
        for w in self.tempo_map.windows(2) {
            if w[1].tick <= w[0].tick {
                return Err(ChartError::BadTempoMap("ticks must be strictly increasing"));
            }
        }
        for p in &self.tempo_map {
            if !(p.bpm.is_finite() && (1.0..=1000.0).contains(&p.bpm)) {
                return Err(ChartError::BadTempo(p.bpm));
            }
        }
        if self.time_sigs.is_empty() {
            return Err(ChartError::EmptyTimeSigs);
        }
        if self.time_sigs[0].tick != 0 {
            return Err(ChartError::BadTimeSigs("first signature must be at tick 0"));
        }
        for w in self.time_sigs.windows(2) {
            if w[1].tick <= w[0].tick {
                return Err(ChartError::BadTimeSigs("ticks must be strictly increasing"));
            }
        }
        for s in &self.time_sigs {
            if s.num == 0 || !matches!(s.den, 1 | 2 | 4 | 8 | 16 | 32) {
                return Err(ChartError::BadTimeSigs("num > 0 and den a power of two"));
            }
        }
        for (name, v) in &self.voicings {
            if v.frets.iter().any(|f| !(-1..=24).contains(f)) {
                return Err(ChartError::BadVoicing(name.clone(), "fret outside -1..=24"));
            }
            if let Some(r) = &v.req
                && (r.chars().count() != 6 || r.chars().any(|c| !"MmXx".contains(c)))
            {
                return Err(ChartError::BadVoicing(
                    name.clone(),
                    "req must be 6 chars of M m X x",
                ));
            }
            if v.root.is_some_and(|r| r > 11) {
                return Err(ChartError::BadVoicing(name.clone(), "root must be 0..=11"));
            }
        }
        for (ti, tier) in self.tiers.iter().enumerate() {
            let mut prev = 0u32;
            for (index, ev) in tier.events.iter().enumerate() {
                let loc = Loc { tier: ti, index };
                if ev.tick < prev {
                    return Err(ChartError::TicksOutOfOrder(loc));
                }
                prev = ev.tick;
                if ev.dur == 0 {
                    return Err(ChartError::ZeroDuration(loc));
                }
                if let Some(c) = &ev.chord
                    && !self.voicings.contains_key(c)
                {
                    return Err(ChartError::UnknownChord(loc, c.clone()));
                }
                match ev.kind {
                    EventKind::Strum { span, .. } => {
                        if ev.chord.is_none() {
                            return Err(ChartError::BadValue(loc, "strum needs a chord"));
                        }
                        if let Some(s) = span {
                            if s.lo > 5 || s.hi > 5 {
                                return Err(ChartError::SpanOutOfRange(loc));
                            }
                            if s.lo > s.hi {
                                return Err(ChartError::SpanReversed(loc));
                            }
                        }
                    }
                    EventKind::Note {
                        string,
                        fret,
                        finger,
                    } => {
                        if string > 5 {
                            return Err(ChartError::BadValue(loc, "string must be 0..=5"));
                        }
                        if fret > 24 {
                            return Err(ChartError::BadValue(loc, "fret must be 0..=24"));
                        }
                        if finger.is_some_and(|c| !"pima".contains(c)) {
                            return Err(ChartError::BadValue(loc, "finger must be p, i, m or a"));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    // ---- JSON -----------------------------------------------------------

    pub fn from_json(s: &str) -> Result<Chart, ChartError> {
        let root: Value = serde_json::from_str(s).map_err(|e| ChartError::Json(e.to_string()))?;
        let o = root
            .as_object()
            .ok_or_else(|| ChartError::NotObject("$".into()))?;
        let v = req(o, "v", "$")?
            .as_i64()
            .ok_or_else(|| ChartError::WrongType("v".into()))?;
        if v != 1 {
            return Err(ChartError::BadVersion(v));
        }
        let tuning = match o.get("tuning") {
            None | Some(Value::Null) => STANDARD_TUNING,
            Some(t) => {
                let a = t
                    .as_array()
                    .filter(|a| a.len() == 6)
                    .ok_or_else(|| ChartError::WrongType("tuning".into()))?;
                let mut out = [0i32; 6];
                for (k, x) in a.iter().enumerate() {
                    out[k] = x
                        .as_i64()
                        .and_then(|n| i32::try_from(n).ok())
                        .ok_or_else(|| ChartError::WrongType("tuning".into()))?;
                }
                out
            }
        };
        let tpq = uint(req(o, "tpq", "$")?, "tpq")?;
        let tempo_map = req(o, "tempo_map", "$")?
            .as_array()
            .ok_or_else(|| ChartError::WrongType("tempo_map".into()))?
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let path = format!("tempo_map[{i}]");
                let p = p
                    .as_object()
                    .ok_or_else(|| ChartError::NotObject(path.clone()))?;
                Ok(TempoPoint {
                    tick: uint(req(p, "tick", &path)?, &format!("{path}.tick"))?,
                    bpm: req(p, "bpm", &path)?
                        .as_f64()
                        .ok_or_else(|| ChartError::WrongType(format!("{path}.bpm")))?,
                })
            })
            .collect::<Result<Vec<_>, ChartError>>()?;
        let time_sigs = req(o, "time_sigs", "$")?
            .as_array()
            .ok_or_else(|| ChartError::WrongType("time_sigs".into()))?
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let path = format!("time_sigs[{i}]");
                let p = p
                    .as_object()
                    .ok_or_else(|| ChartError::NotObject(path.clone()))?;
                let small = |k: &str| -> Result<u8, ChartError> {
                    u8::try_from(uint(req(p, k, &path)?, &format!("{path}.{k}"))?)
                        .map_err(|_| ChartError::WrongType(format!("{path}.{k}")))
                };
                Ok(TimeSig {
                    tick: uint(req(p, "tick", &path)?, &format!("{path}.tick"))?,
                    num: small("num")?,
                    den: small("den")?,
                })
            })
            .collect::<Result<Vec<_>, ChartError>>()?;

        let mut voicings = BTreeMap::new();
        if let Some(Value::Object(m)) = o.get("voicings") {
            for (name, v) in m {
                voicings.insert(name.clone(), parse_voicing(name, v)?);
            }
        }
        let mut patterns = BTreeMap::new();
        if let Some(Value::Object(m)) = o.get("patterns") {
            for (name, v) in m {
                let path = format!("patterns.{name}");
                let p = v
                    .as_object()
                    .ok_or_else(|| ChartError::NotObject(path.clone()))?;
                let cells = req(p, "cells", &path)?
                    .as_array()
                    .ok_or_else(|| ChartError::WrongType(format!("{path}.cells")))?
                    .iter()
                    .map(|c| {
                        c.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| ChartError::WrongType(format!("{path}.cells")))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                patterns.insert(
                    name.clone(),
                    Pattern {
                        grid: uint(req(p, "grid", &path)?, &format!("{path}.grid"))?,
                        cells,
                    },
                );
            }
        }
        let mut sections = Vec::new();
        if let Some(Value::Array(a)) = o.get("sections") {
            for (i, s) in a.iter().enumerate() {
                let path = format!("sections[{i}]");
                let s = s
                    .as_object()
                    .ok_or_else(|| ChartError::NotObject(path.clone()))?;
                sections.push(Section {
                    id: str_or(s, "id"),
                    name: str_or(s, "name"),
                    start: uint(req(s, "start", &path)?, &format!("{path}.start"))?,
                    end: uint(req(s, "end", &path)?, &format!("{path}.end"))?,
                });
            }
        }
        let mut tiers = Vec::new();
        if let Some(Value::Array(a)) = o.get("tiers") {
            for (ti, t) in a.iter().enumerate() {
                let path = format!("tiers[{ti}]");
                let t = t
                    .as_object()
                    .ok_or_else(|| ChartError::NotObject(path.clone()))?;
                let evs = req(t, "events", &path)?
                    .as_array()
                    .ok_or_else(|| ChartError::WrongType(format!("{path}.events")))?;
                let mut events = Vec::with_capacity(evs.len());
                for (index, e) in evs.iter().enumerate() {
                    events.push(parse_event(e, Loc { tier: ti, index })?);
                }
                tiers.push(Tier {
                    tier: t
                        .get("tier")
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(0),
                    name: str_or(t, "name"),
                    events,
                });
            }
        }
        let chart = Chart {
            v: 1,
            id: str_or(o, "id"),
            title: str_or(o, "title"),
            origin: o
                .get("origin")
                .and_then(Value::as_str)
                .unwrap_or("own")
                .to_string(),
            tuning,
            capo: o
                .get("capo")
                .and_then(Value::as_u64)
                .and_then(|n| u8::try_from(n).ok())
                .unwrap_or(0),
            tpq,
            tempo_map,
            time_sigs,
            audio: o.get("audio").and_then(Value::as_str).map(str::to_string),
            voicings,
            patterns,
            sections,
            tiers,
            lyrics: o
                .get("lyrics")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        };
        chart.validate()?;
        Ok(chart)
    }

    /// Compact single-line JSON (what the tape stores).
    pub fn to_json(&self) -> String {
        self.to_value().to_string()
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.to_value()).unwrap_or_default()
    }

    fn to_value(&self) -> Value {
        let voicings: Map<String, Value> = self
            .voicings
            .iter()
            .map(|(k, v)| {
                let mut m = Map::new();
                m.insert("frets".into(), json!(v.frets));
                if let Some(r) = &v.req {
                    m.insert("req".into(), json!(r));
                }
                m.insert("fingers".into(), json!(v.fingers));
                if let Some(r) = v.root {
                    m.insert("root".into(), json!(r));
                }
                (k.clone(), Value::Object(m))
            })
            .collect();
        let patterns: Map<String, Value> = self
            .patterns
            .iter()
            .map(|(k, p)| (k.clone(), json!({ "grid": p.grid, "cells": p.cells })))
            .collect();
        json!({
            "v": self.v,
            "id": self.id,
            "title": self.title,
            "origin": self.origin,
            "tuning": self.tuning,
            "capo": self.capo,
            "tpq": self.tpq,
            "tempo_map": self.tempo_map.iter().map(|p| json!({
                "tick": p.tick,
                "bpm": if p.bpm.fract() == 0.0 && p.bpm.abs() < 1e9 { json!(p.bpm as i64) } else { json!(p.bpm) },
            })).collect::<Vec<_>>(),
            "time_sigs": self.time_sigs.iter().map(|s| json!({
                "tick": s.tick, "num": s.num, "den": s.den,
            })).collect::<Vec<_>>(),
            "audio": self.audio,
            "voicings": voicings,
            "patterns": patterns,
            "sections": self.sections.iter().map(|s| json!({
                "id": s.id, "name": s.name, "start": s.start, "end": s.end,
            })).collect::<Vec<_>>(),
            "tiers": self.tiers.iter().map(|t| json!({
                "tier": t.tier,
                "name": t.name,
                "events": t.events.iter().map(event_value).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "lyrics": self.lyrics,
        })
    }
}

/// Root pitch class from a chord name such as `G`, `Em`, `F#m7`, `Bb`.
pub fn root_pc_of_name(name: &str) -> Option<u8> {
    let mut it = name.chars();
    let base: i32 = match it.next()? {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let acc = match it.next() {
        Some('#') => 1,
        Some('b') => -1,
        _ => 0,
    };
    Some((base + acc).rem_euclid(12) as u8)
}

fn req<'a>(o: &'a Map<String, Value>, k: &str, path: &str) -> Result<&'a Value, ChartError> {
    o.get(k).ok_or_else(|| {
        ChartError::MissingField(if path == "$" {
            k.to_string()
        } else {
            format!("{path}.{k}")
        })
    })
}

fn uint(v: &Value, path: &str) -> Result<u32, ChartError> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| ChartError::WrongType(path.to_string()))
}

fn str_or(o: &Map<String, Value>, k: &str) -> String {
    o.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn parse_voicing(name: &str, v: &Value) -> Result<Voicing, ChartError> {
    let path = format!("voicings.{name}");
    let o = v
        .as_object()
        .ok_or_else(|| ChartError::NotObject(path.clone()))?;
    let frets_v = req(o, "frets", &path)?
        .as_array()
        .filter(|a| a.len() == 6)
        .ok_or_else(|| ChartError::WrongType(format!("{path}.frets")))?;
    let mut frets = [0i8; 6];
    for (k, x) in frets_v.iter().enumerate() {
        frets[k] = x
            .as_i64()
            .and_then(|n| i8::try_from(n).ok())
            .ok_or_else(|| ChartError::WrongType(format!("{path}.frets")))?;
    }
    let mut fingers = [0u8; 6];
    if let Some(a) = o.get("fingers") {
        let a = a
            .as_array()
            .filter(|a| a.len() == 6)
            .ok_or_else(|| ChartError::WrongType(format!("{path}.fingers")))?;
        for (k, x) in a.iter().enumerate() {
            fingers[k] = x
                .as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| ChartError::WrongType(format!("{path}.fingers")))?;
        }
    }
    Ok(Voicing {
        frets,
        req: o.get("req").and_then(Value::as_str).map(str::to_string),
        fingers,
        root: o
            .get("root")
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok()),
    })
}

fn parse_dir(o: &Map<String, Value>, loc: Loc) -> Result<Dir, ChartError> {
    match o.get("dir") {
        None => Ok(Dir::Down),
        Some(Value::String(s)) if s == "D" => Ok(Dir::Down),
        Some(Value::String(s)) if s == "U" => Ok(Dir::Up),
        Some(_) => Err(ChartError::BadValue(loc, "dir must be \"D\" or \"U\"")),
    }
}

fn parse_event(e: &Value, loc: Loc) -> Result<Event, ChartError> {
    let o = e.as_object().ok_or_else(|| {
        ChartError::NotObject(format!("tiers[{}].events[{}]", loc.tier, loc.index))
    })?;
    let path = format!("tiers[{}].events[{}]", loc.tier, loc.index);
    let tick = uint(req(o, "tick", &path)?, &format!("{path}.tick"))?;
    let dur = uint(req(o, "dur", &path)?, &format!("{path}.dur"))?;
    let kind_s = req(o, "kind", &path)?
        .as_str()
        .ok_or_else(|| ChartError::WrongType(format!("{path}.kind")))?;
    let chord = o.get("chord").and_then(Value::as_str).map(str::to_string);
    let kind = match kind_s {
        "strum" => {
            let span = match o.get("span") {
                None | Some(Value::Null) => None,
                Some(s) => {
                    let a = s
                        .as_array()
                        .filter(|a| a.len() == 2)
                        .ok_or_else(|| ChartError::WrongType(format!("{path}.span")))?;
                    let n = |x: &Value| -> Result<u8, ChartError> {
                        // Out-of-range numbers (including > 255) are range errors, not type errors.
                        x.as_u64()
                            .map(|n| n.min(255) as u8)
                            .ok_or_else(|| ChartError::WrongType(format!("{path}.span")))
                    };
                    Some(Span {
                        lo: n(&a[0])?,
                        hi: n(&a[1])?,
                    })
                }
            };
            let style = match o.get("style").and_then(Value::as_str) {
                None | Some("full") => Style::Full,
                Some("accent") => Style::Accent,
                Some("roll") => Style::Roll,
                Some("pm") => Style::Pm,
                Some(_) => return Err(ChartError::BadValue(loc, "unknown style")),
            };
            EventKind::Strum {
                dir: parse_dir(o, loc)?,
                span,
                style,
                roll_ms: o
                    .get("roll_ms")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
            }
        }
        "mute" => EventKind::Mute {
            dir: parse_dir(o, loc)?,
        },
        "ghost" => EventKind::Ghost {
            dir: parse_dir(o, loc)?,
        },
        "note" => {
            let small = |k: &str| -> Result<u8, ChartError> {
                req(o, k, &path)?
                    .as_u64()
                    .map(|n| n.min(255) as u8)
                    .ok_or_else(|| ChartError::WrongType(format!("{path}.{k}")))
            };
            let finger = match o.get("finger") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if s.chars().count() == 1 => s.chars().next(),
                Some(_) => return Err(ChartError::BadValue(loc, "finger must be one letter")),
            };
            EventKind::Note {
                string: small("string")?,
                fret: small("fret")?,
                finger,
            }
        }
        "rest" => EventKind::Rest,
        other => return Err(ChartError::UnknownKind(loc, other.to_string())),
    };
    Ok(Event {
        tick,
        dur,
        kind,
        chord,
    })
}

fn event_value(e: &Event) -> Value {
    let mut m = Map::new();
    m.insert("tick".into(), json!(e.tick));
    m.insert("dur".into(), json!(e.dur));
    match e.kind {
        EventKind::Strum {
            dir,
            span,
            style,
            roll_ms,
        } => {
            m.insert("kind".into(), json!("strum"));
            m.insert("dir".into(), json!(dir.as_str()));
            if let Some(s) = span {
                m.insert("span".into(), json!([s.lo, s.hi]));
            }
            if style != Style::Full {
                m.insert("style".into(), json!(style.as_str()));
            }
            if let Some(r) = roll_ms {
                m.insert("roll_ms".into(), json!(r));
            }
        }
        EventKind::Mute { dir } => {
            m.insert("kind".into(), json!("mute"));
            m.insert("dir".into(), json!(dir.as_str()));
        }
        EventKind::Ghost { dir } => {
            m.insert("kind".into(), json!("ghost"));
            m.insert("dir".into(), json!(dir.as_str()));
        }
        EventKind::Note {
            string,
            fret,
            finger,
        } => {
            m.insert("kind".into(), json!("note"));
            m.insert("string".into(), json!(string));
            m.insert("fret".into(), json!(fret));
            if let Some(f) = finger {
                m.insert("finger".into(), json!(f.to_string()));
            }
        }
        EventKind::Rest => {
            m.insert("kind".into(), json!("rest"));
        }
    }
    if let Some(c) = &e.chord {
        m.insert("chord".into(), json!(c));
    }
    Value::Object(m)
}
