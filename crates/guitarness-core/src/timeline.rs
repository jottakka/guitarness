//! Beat and time conversions over a chart: the game's drawable timeline
//! (events in seconds, glyphs, chords), built from a [`Chart`].
//!
//! Replaces the prototype's hardcoded `guitarness-game::chart::Chart`. Names follow
//! chart.json v1; `Style` extends `strum` with the stroke variants.

use guitarness_theory::interval::Interval;

use crate::chart::{Chart, EventKind, Section, Span, Style as ChartStyle, Voicing};
pub use crate::chart::{Dir, STANDARD_TUNING};

/// Open-string MIDI notes of standard tuning (string 0 = 6th).
pub const OPEN: [i32; 6] = STANDARD_TUNING;

/// Seconds between strings of a roll when the chart gives no `roll_ms`.
pub const ROLL_STEP_S: f32 = 0.09;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Normal,
    /// Chuck: all strings damped, no pitch expected (chart kind `mute`).
    Mute,
    Accent,
    /// Thumb on the bass strings (span 0..=2).
    Bass,
    /// Fingers on the treble strings (span 3..=5).
    Treble,
    /// Slow rolled strum / arpeggio.
    Roll,
    /// Air stroke: hand moves, strings untouched, not judged.
    Ghost,
    PalmMute,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub name: String,
    pub root_pc: i32,
    /// Fret per string, -1 = not played.
    pub frets: [i32; 6],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Strum {
        dir: Dir,
        style: Style,
    },
    Note {
        string: usize,
        fret: i32,
        finger: Option<char>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub t: f32,
    /// Sustain length in seconds (0 = none). Only notes and palm mutes sustain.
    pub dur: f32,
    pub chord: usize,
    pub kind: Kind,
    /// Chord symbol shown above the first event after a chord change.
    pub label: Option<String>,
    /// Strings the stroke reaches, inclusive, before the voicing is applied.
    pub range: (usize, usize),
    /// Roll spread: seconds between consecutive strings (rolls only).
    pub roll_step: f32,
    /// Tick in the source chart, so judge ids can be matched back.
    pub tick: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    pub string: usize,
    pub fret: i32,
    pub interval: Interval,
    /// Time offset of this note inside a rolled strum.
    pub dt: f32,
    pub finger: Option<char>,
}

#[derive(Clone, Debug)]
pub struct Timeline {
    pub name: String,
    /// Seconds per beat at the first tempo.
    pub beat: f32,
    /// Travel speed multiplier (the gallery runs slower so more styles fit).
    pub speed: f32,
    pub tuning: [i32; 6],
    pub chords: Vec<Chord>,
    pub events: Vec<Event>,
    pub length: f32,
}

impl Timeline {
    /// Build the drawable timeline from the first tier of `c`. Rests are
    /// dropped. Events without a chord inherit the previous one; if the chart
    /// has none, an anonymous chord with all strings open is used.
    pub fn from_chart(c: &Chart) -> Timeline {
        let mut chords: Vec<Chord> = Vec::new();
        let index_of = |name: &str, c: &Chart, chords: &mut Vec<Chord>| -> usize {
            if let Some(i) = chords.iter().position(|x| x.name == name) {
                return i;
            }
            let frets = c.voicings.get(name).map_or([0; 6], |v: &Voicing| {
                let mut f = [0i32; 6];
                for (k, x) in v.frets.iter().enumerate() {
                    f[k] = i32::from(*x);
                }
                f
            });
            chords.push(Chord {
                name: name.to_string(),
                root_pc: i32::from(c.root_pc(name)),
                frets,
            });
            chords.len() - 1
        };
        let mut events = Vec::new();
        let mut current: Option<usize> = None;
        let mut labelled: Option<usize> = None;
        for e in c.events() {
            if matches!(e.kind, EventKind::Rest) {
                continue;
            }
            let ci = match &e.chord {
                Some(n) => {
                    let i = index_of(n, c, &mut chords);
                    current = Some(i);
                    i
                }
                None => match current {
                    Some(i) => i,
                    None => {
                        let i = index_of("", c, &mut chords);
                        current = Some(i);
                        i
                    }
                },
            };
            let label = (labelled != Some(ci) && !chords[ci].name.is_empty())
                .then(|| chords[ci].name.clone());
            if label.is_some() {
                labelled = Some(ci);
            }
            let t = c.seconds_of_tick(e.tick) as f32;
            let sec = |ticks: u32| (c.seconds_of_tick(e.tick + ticks) as f32) - t;
            let full = |dir: Dir| match dir {
                Dir::Down => (0usize, 5usize),
                Dir::Up => (2, 5),
            };
            let (kind, dur, range, roll_step) = match e.kind {
                EventKind::Strum {
                    dir,
                    span,
                    style,
                    roll_ms,
                } => {
                    let range =
                        span.map_or(full(dir), |Span { lo, hi }| (lo as usize, hi as usize));
                    let st = match style {
                        ChartStyle::Accent => Style::Accent,
                        ChartStyle::Roll => Style::Roll,
                        ChartStyle::Pm => Style::PalmMute,
                        ChartStyle::Full => match span {
                            Some(Span { lo: 0, hi: 2 }) => Style::Bass,
                            Some(Span { lo: 3, hi: 5 }) => Style::Treble,
                            _ => Style::Normal,
                        },
                    };
                    let n = chords[ci]
                        .frets
                        .iter()
                        .enumerate()
                        .filter(|(s, f)| **f >= 0 && *s >= range.0 && *s <= range.1)
                        .count();
                    let step = match roll_ms {
                        Some(ms) if n > 1 => ms as f32 / 1000.0 / (n - 1) as f32,
                        Some(_) => 0.0,
                        None => ROLL_STEP_S,
                    };
                    let dur = if st == Style::PalmMute {
                        sec(e.dur)
                    } else {
                        0.0
                    };
                    (Kind::Strum { dir, style: st }, dur, range, step)
                }
                EventKind::Mute { dir } => (
                    Kind::Strum {
                        dir,
                        style: Style::Mute,
                    },
                    0.0,
                    (0, 5),
                    0.0,
                ),
                EventKind::Ghost { dir } => (
                    Kind::Strum {
                        dir,
                        style: Style::Ghost,
                    },
                    0.0,
                    full(dir),
                    0.0,
                ),
                EventKind::Note {
                    string,
                    fret,
                    finger,
                } => (
                    Kind::Note {
                        string: string as usize,
                        fret: i32::from(fret),
                        finger,
                    },
                    sec(e.dur),
                    (string as usize, string as usize),
                    0.0,
                ),
                EventKind::Rest => continue,
            };
            events.push(Event {
                t,
                dur,
                chord: ci,
                kind,
                label,
                range,
                roll_step,
                tick: e.tick,
            });
        }
        events.sort_by(|a, b| a.t.total_cmp(&b.t));
        let end_tick = c
            .sections
            .iter()
            .map(|s: &Section| s.end)
            .max()
            .or_else(|| c.events().iter().map(|e| e.tick + e.dur).max())
            .unwrap_or(0);
        let beat = 60.0 / c.bpm_at(0) as f32;
        Timeline {
            name: c.id.clone(),
            beat,
            speed: 1.0,
            tuning: c.tuning,
            chords,
            events,
            length: c.seconds_of_tick(end_tick) as f32,
        }
    }

    /// The prototype's demo song: G C D Em, D-DU-UDU at 80 BPM plus notes.
    pub fn demo() -> Timeline {
        Timeline::from_chart(&demo_chart())
    }

    /// Every stroke style in order, 70 BPM, three beats per style.
    pub fn gallery() -> Timeline {
        let mut t = Timeline::from_chart(&gallery_chart());
        t.speed = 0.55;
        t
    }

    /// Strings the stroke covers (frame / arrow span).
    pub fn covered(&self, ev: &Event) -> [bool; 6] {
        let mut out = [false; 6];
        match ev.kind {
            Kind::Strum { .. } => {
                for (s, f) in self.chords[ev.chord].frets.iter().enumerate() {
                    out[s] = *f >= 0 && s >= ev.range.0 && s <= ev.range.1;
                }
            }
            Kind::Note { string, .. } => out[string] = true,
        }
        out
    }

    /// (lowest, highest) covered string.
    pub fn span(&self, ev: &Event) -> Option<(usize, usize)> {
        let c = self.covered(ev);
        let lo = (0..6).find(|s| c[*s])?;
        let hi = (0..6).rev().find(|s| c[*s])?;
        Some((lo, hi))
    }

    pub fn is_ghost(ev: &Event) -> bool {
        matches!(
            ev.kind,
            Kind::Strum {
                style: Style::Ghost,
                ..
            }
        )
    }

    pub fn is_mute(ev: &Event) -> bool {
        matches!(
            ev.kind,
            Kind::Strum {
                style: Style::Mute,
                ..
            }
        )
    }

    /// Ghost strokes are never judged.
    pub fn judged(ev: &Event) -> bool {
        !Self::is_ghost(ev)
    }

    /// Strings that must ring (none for chucks and ghosts).
    pub fn expected(&self, ev: &Event) -> [bool; 6] {
        if Self::is_ghost(ev) || Self::is_mute(ev) {
            [false; 6]
        } else {
            self.covered(ev)
        }
    }

    /// Pitched glyphs of an event (empty for chucks and ghosts). Up strokes
    /// list the high strings first.
    pub fn glyphs(&self, ev: &Event) -> Vec<Glyph> {
        let chord = &self.chords[ev.chord];
        let mask = self.expected(ev);
        let order: Vec<usize> = match ev.kind {
            Kind::Strum { dir: Dir::Up, .. } => (0..6).rev().collect(),
            _ => (0..6).collect(),
        };
        let roll = matches!(
            ev.kind,
            Kind::Strum {
                style: Style::Roll,
                ..
            }
        );
        let mut n = 0;
        let mut out = Vec::new();
        for s in order {
            if !mask[s] {
                continue;
            }
            let (fret, finger) = match ev.kind {
                Kind::Note { fret, finger, .. } => (fret, finger),
                Kind::Strum { .. } => (chord.frets[s], None),
            };
            let midi = self.tuning[s] + fret;
            out.push(Glyph {
                string: s,
                fret,
                interval: Interval::from_semitones(midi - chord.root_pc),
                dt: if roll { n as f32 * ev.roll_step } else { 0.0 },
                finger,
            });
            n += 1;
        }
        out
    }
}

fn voicing(frets: [i8; 6], root: u8) -> Voicing {
    Voicing {
        frets,
        req: None,
        fingers: [0; 6],
        root: Some(root),
    }
}

fn demo_voicings(c: &mut Chart) {
    for (n, f, r) in [
        ("G", [3, 2, 0, 0, 0, 3], 7),
        ("C", [-1, 3, 2, 0, 1, 0], 0),
        ("D", [-1, -1, 0, 2, 3, 2], 2),
        ("Em", [0, 2, 2, 0, 0, 0], 4),
    ] {
        c.voicings.insert(n.to_string(), voicing(f, r));
    }
}

fn push(c: &mut Chart, tick: u32, dur: u32, kind: EventKind, chord: Option<&str>) {
    c.tiers[0].events.push(crate::chart::Event {
        tick,
        dur,
        kind,
        chord: chord.map(str::to_string),
    });
}

fn strum(dir: Dir, span: Option<Span>, style: ChartStyle) -> EventKind {
    EventKind::Strum {
        dir,
        span,
        style,
        roll_ms: None,
    }
}

/// The demo song as a chart: G C D Em, D-DU-UDU at 80 BPM plus a few notes.
pub fn demo_chart() -> Chart {
    let mut c = Chart::new("demo", 80.0);
    c.title = "Demo".into();
    demo_voicings(&mut c);
    let names = ["G", "C", "D", "Em"];
    let pattern = [
        Some(Dir::Down),
        None,
        Some(Dir::Down),
        Some(Dir::Up),
        None,
        Some(Dir::Up),
        Some(Dir::Down),
        Some(Dir::Up),
    ];
    // (bar, cell, string, fret) single notes on the empty cells.
    let notes = [
        (1, 1, 0, 3),
        (1, 4, 2, 2),
        (2, 1, 1, 0),
        (2, 4, 3, 0),
        (3, 1, 5, 0),
        (3, 4, 3, 0),
    ];
    for (b, name) in names.iter().enumerate() {
        for (cell, dir) in pattern.iter().enumerate() {
            if let Some(dir) = dir {
                let tick = b as u32 * 1920 + cell as u32 * 240;
                push(
                    &mut c,
                    tick,
                    240,
                    strum(*dir, None, ChartStyle::Full),
                    Some(name),
                );
            }
        }
    }
    for (b, cell, string, fret) in notes {
        let tick = b * 1920 + cell * 240;
        push(
            &mut c,
            tick,
            384,
            EventKind::Note {
                string: string as u8,
                fret: fret as u8,
                finger: None,
            },
            Some(names[b as usize]),
        );
    }
    c.tiers[0].events.sort_by_key(|e| e.tick);
    c.sections.push(Section {
        id: "demo".into(),
        name: "Demo".into(),
        start: 0,
        end: 4 * 1920,
    });
    c
}

/// Every stroke style as a chart, 70 BPM, a group of three beats per style.
pub fn gallery_chart() -> Chart {
    let mut c = Chart::new("strokes", 70.0);
    c.title = "Strokes".into();
    demo_voicings(&mut c);
    let names = ["G", "C", "D", "Em"];
    let full = ChartStyle::Full;
    let sp = |lo, hi| Some(Span { lo, hi });
    use Dir::{Down, Up};
    // (group, beats, chord, event kind)
    let mute = |d| EventKind::Mute { dir: d };
    let rows: Vec<(u32, f32, usize, EventKind)> = vec![
        (0, 0.0, 0, strum(Down, None, full)),
        (0, 1.0, 0, strum(Up, None, full)),
        (1, 0.0, 1, mute(Down)),
        (1, 1.0, 1, mute(Down)),
        (2, 0.0, 2, strum(Down, None, ChartStyle::Accent)),
        (2, 1.0, 2, strum(Down, None, full)),
        (3, 0.0, 0, strum(Down, sp(0, 2), full)),
        (3, 1.0, 0, strum(Down, sp(3, 5), full)),
        (4, 0.0, 1, strum(Down, None, ChartStyle::Roll)),
        (4, 1.0, 1, strum(Up, None, ChartStyle::Roll)),
        (5, 0.0, 2, strum(Down, None, full)),
        (5, 1.0, 2, EventKind::Ghost { dir: Up }),
        (6, 0.0, 3, strum(Down, None, ChartStyle::Pm)),
        (6, 0.75, 3, strum(Down, None, ChartStyle::Pm)),
        (6, 1.5, 3, strum(Down, None, ChartStyle::Pm)),
    ];
    for (g, beats, chord, kind) in rows {
        let tick = ((g * 3) as f32 * 480.0 + beats * 480.0) as u32;
        let dur = if matches!(
            kind,
            EventKind::Strum {
                style: ChartStyle::Pm,
                ..
            }
        ) {
            120
        } else {
            240
        };
        push(&mut c, tick, dur, kind, Some(names[chord]));
    }
    // Fingerpicking on C: p i m a, thumb on the bass strings.
    for (b, string, fret, finger) in [
        (0.0, 1, 3, 'p'),
        (0.5, 3, 0, 'i'),
        (1.0, 4, 1, 'm'),
        (1.5, 5, 0, 'a'),
    ] {
        let tick = (7.0 * 3.0 * 480.0 + b * 480.0) as u32;
        push(
            &mut c,
            tick,
            240,
            EventKind::Note {
                string,
                fret,
                finger: Some(finger),
            },
            Some("C"),
        );
    }
    c.sections.push(Section {
        id: "strokes".into(),
        name: "Strokes".into(),
        start: 0,
        end: 24 * 480,
    });
    c
}
