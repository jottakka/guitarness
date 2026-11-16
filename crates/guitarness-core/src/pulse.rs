//! Pulse exercises built from a chart (owned by C1): the click exercises and
//! the list of expectations the judge is fed.

use crate::chart::{Chart, Dir, Event, EventKind, Section};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exercise {
    /// Muted quarter notes on the click.
    OnClickQuarters,
    /// Muted eighths, down on the beat and up between.
    OnClickEighths,
}

pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 240.0;

/// A looping, original click exercise: 4/4, tpq 480, `bars` bars (at least 1),
/// `bpm` clamped to 20..=240. Chuck strokes only, no voicings.
pub fn exercise(kind: Exercise, bpm: f64, bars: u32) -> Chart {
    let bars = bars.max(1);
    let bpm = if bpm.is_finite() {
        bpm.clamp(MIN_BPM, MAX_BPM)
    } else {
        80.0
    };
    let (id, title, step) = match kind {
        Exercise::OnClickQuarters => ("on-click-quarters", "No clique: seminimas", 480u32),
        Exercise::OnClickEighths => ("on-click-eighths", "No clique: colcheias", 240u32),
    };
    let mut c = Chart::new(id, bpm);
    c.title = title.to_string();
    let total = bars * 1920;
    let mut tick = 0;
    let mut n = 0u32;
    while tick < total {
        let dir = if n % 2 == 1 && step == 240 {
            Dir::Up
        } else {
            Dir::Down
        };
        c.tiers[0].events.push(Event {
            tick,
            dur: step,
            kind: EventKind::Mute { dir },
            chord: None,
        });
        tick += step;
        n += 1;
    }
    c.sections.push(Section {
        id: "round".into(),
        name: "Rodada".into(),
        start: 0,
        end: total,
    });
    c
}

/// One thing the player is expected to do (or, for a ghost, may do).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Expectation {
    /// Sequential from 0 in event order; the judge's `id`.
    pub id: u32,
    pub tick: u32,
    pub t_s: f64,
    /// Ghost strokes are expected as hand motion only and never judged.
    pub ghost: bool,
    /// Seconds to the previous / next expectation. The first has
    /// `prev_gap_s == next_gap_s` and the last `next_gap_s == prev_gap_s`; a
    /// lone one uses one beat.
    pub prev_gap_s: f64,
    pub next_gap_s: f64,
    /// Eighth-note slot inside the bar (4/4: 0..=7).
    pub pos_in_bar: u8,
}

/// Expectations of the chart's first tier: every event but rests.
pub fn expectations(c: &Chart) -> Vec<Expectation> {
    let evs: Vec<&Event> = c
        .events()
        .iter()
        .filter(|e| !matches!(e.kind, EventKind::Rest))
        .collect();
    let times: Vec<f64> = evs.iter().map(|e| c.seconds_of_tick(e.tick)).collect();
    let beat = 60.0 / c.bpm_at(0);
    (0..evs.len())
        .map(|k| {
            let prev = (k > 0).then(|| times[k] - times[k - 1]);
            let next = (k + 1 < evs.len()).then(|| times[k + 1] - times[k]);
            let (p, n) = match (prev, next) {
                (Some(p), Some(n)) => (p, n),
                (Some(p), None) => (p, p),
                (None, Some(n)) => (n, n),
                (None, None) => (beat, beat),
            };
            Expectation {
                id: k as u32,
                tick: evs[k].tick,
                t_s: times[k],
                ghost: matches!(evs[k].kind, EventKind::Ghost { .. }),
                prev_gap_s: p,
                next_gap_s: n,
                pos_in_bar: c.eighth_in_bar(evs[k].tick),
            }
        })
        .collect()
}
