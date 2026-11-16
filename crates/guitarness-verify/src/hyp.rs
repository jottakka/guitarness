//! Closed confusion set of a chart strum (the verifier/learner design plan section 2.4, first part).
//!
//! The hypothesis set comes from the chart only: the voicing, its `req` letters, the stroke
//! `span` and the neighbouring chords. Nothing is transcribed from audio.

use guitarness_core::chart::{Chart, Dir, Event, EventKind, Loc, Voicing};

use crate::types::{Ask, ConfusionSet, HypKind, Hypothesis};

/// Hard ceiling on `alts`. Lowest-priority hypotheses (`WrongFret`) are dropped first.
pub const MAX_ALTS: usize = 24;

/// Highest fret kept in a `WrongFret` hypothesis (same limit as chart validation).
const MAX_FRET: i32 = 24;

/// Strings a stroke reaches, inclusive: explicit `span`, else down 0..=5 and up 2..=5.
fn reach(dir: Dir, span: Option<guitarness_core::chart::Span>) -> (usize, usize) {
    match span {
        Some(s) => (usize::from(s.lo), usize::from(s.hi)),
        None => match dir {
            Dir::Down => (0, 5),
            Dir::Up => (2, 5),
        },
    }
}

/// Per-string ask for a voicing under a stroke reaching `lo..=hi`.
///
/// Letters: `M` Must, `m` May, `X` MustNot, `x` May (MustNot when `strict`). Without `req`, a
/// fretted/open string is Must and an unplayed one is MustNot. Strings outside the reach are May:
/// the stroke never touches them, so nothing is required of them or forbidden. A `M` letter on an
/// unplayed string (malformed voicing) degrades to May rather than inventing a fret.
fn asks(v: &Voicing, lo: usize, hi: usize, strict: bool) -> [Ask; 6] {
    let letters: Option<Vec<char>> = v
        .req
        .as_ref()
        .map(|r| r.chars().collect::<Vec<_>>())
        .filter(|c| c.len() == 6);
    let mut out = [Ask::May; 6];
    for (s, slot) in out.iter_mut().enumerate() {
        if s < lo || s > hi {
            continue;
        }
        let fret = v.frets[s];
        let letter = match &letters {
            Some(l) => l[s],
            None if fret >= 0 => 'M',
            None => 'X',
        };
        *slot = match letter {
            'M' if fret >= 0 => Ask::Must(fret as u8),
            'X' => Ask::MustNot,
            'x' if strict => Ask::MustNot,
            _ => Ask::May,
        };
    }
    out
}

/// What the expected stroke sounds: Must strings, plus May strings that are fretted and reached.
fn expected_sounding(v: &Voicing, ask: &[Ask; 6], lo: usize, hi: usize) -> [i8; 6] {
    let mut out = [-1i8; 6];
    for s in 0..6 {
        out[s] = match ask[s] {
            Ask::Must(f) => f as i8,
            Ask::May if (lo..=hi).contains(&s) => v.frets[s].max(-1),
            Ask::May => -1,
            Ask::MustNot => -1,
        };
    }
    out
}

/// Neighbour hypothesis: the same stroke played on another chord's voicing.
fn neighbour(chart: &Chart, name: &str, lo: usize, hi: usize) -> Option<Hypothesis> {
    let v = chart.voicings.get(name)?;
    let mut sounding = [-1i8; 6];
    for (s, slot) in sounding.iter_mut().enumerate() {
        if (lo..=hi).contains(&s) {
            *slot = v.frets[s].max(-1);
        }
    }
    Some(Hypothesis {
        kind: HypKind::Neighbour {
            chord: name.to_string(),
        },
        sounding,
    })
}

fn strum_chord(e: &Event) -> Option<&str> {
    match e.kind {
        EventKind::Strum { .. } => e.chord.as_deref(),
        _ => None,
    }
}

/// Closed hypothesis set for the strum at `loc`; `None` for anything that is not a pitched strum
/// (`Mute`, `Ghost`, `Rest`, `Note`), for an unknown voicing, and for a strum that requires no
/// string at all (nothing to verify).
///
/// `strict` turns `x` letters into `MustNot` (Banda level); otherwise they are `May`.
pub fn confusion_set(chart: &Chart, loc: Loc, strict: bool) -> Option<ConfusionSet> {
    let tier = chart.tiers.get(loc.tier)?;
    let ev = tier.events.get(loc.index)?;
    let EventKind::Strum { dir, span, .. } = ev.kind else {
        return None;
    };
    let name = ev.chord.as_deref()?;
    let v = chart.voicings.get(name)?;
    let (lo, hi) = reach(dir, span);
    let ask = asks(v, lo, hi, strict);
    if !ask.iter().any(|a| matches!(a, Ask::Must(_))) {
        return None;
    }
    let base = expected_sounding(v, &ask, lo, hi);
    let expected = Hypothesis {
        kind: HypKind::Expected,
        sounding: base,
    };

    let mut missing = Vec::new();
    let mut wrong = Vec::new();
    let mut extra = Vec::new();
    for s in 0..6 {
        match ask[s] {
            Ask::Must(f) => {
                let mut m = base;
                m[s] = -1;
                missing.push(Hypothesis {
                    kind: HypKind::Missing { string: s as u8 },
                    sounding: m,
                });
                for alt in [i32::from(f) - 1, i32::from(f) + 1, 0] {
                    if !(0..=MAX_FRET).contains(&alt) || alt == i32::from(f) {
                        continue;
                    }
                    let mut w = base;
                    w[s] = alt as i8;
                    wrong.push(Hypothesis {
                        kind: HypKind::WrongFret {
                            string: s as u8,
                            fret: alt as u8,
                        },
                        sounding: w,
                    });
                }
            }
            Ask::MustNot => {
                let mut x = base;
                x[s] = 0;
                extra.push(Hypothesis {
                    kind: HypKind::Extra { string: s as u8 },
                    sounding: x,
                });
            }
            Ask::May => {}
        }
    }

    // Previous and next *different* chord named by a strum of the same tier.
    let prev = tier.events[..loc.index]
        .iter()
        .rev()
        .filter_map(strum_chord)
        .find(|c| *c != name);
    let next = tier.events[loc.index + 1..]
        .iter()
        .filter_map(strum_chord)
        .find(|c| *c != name);
    let neighbours: Vec<Hypothesis> = [prev, next]
        .into_iter()
        .flatten()
        .filter_map(|c| neighbour(chart, c, lo, hi))
        .collect();
    let muted = Hypothesis {
        kind: HypKind::AllMuted,
        sounding: [-1; 6],
    };

    // Plan order: Missing, WrongFret, Extra, Neighbour, AllMuted. Dedupe by `sounding` (the
    // expected sounding included); over the ceiling, WrongFret is the first to go.
    let mut seen = vec![base];
    let mut keep = |hs: Vec<Hypothesis>| -> Vec<Hypothesis> {
        hs.into_iter()
            .filter(|h| {
                if seen.contains(&h.sounding) {
                    false
                } else {
                    seen.push(h.sounding);
                    true
                }
            })
            .collect()
    };
    let missing = keep(missing);
    let mut wrong = keep(wrong);
    let extra = keep(extra);
    let neighbours = keep(neighbours);
    let muted = keep(vec![muted]);
    let fixed = missing.len() + extra.len() + neighbours.len() + muted.len();
    wrong.truncate(MAX_ALTS.saturating_sub(fixed));

    let mut alts = Vec::with_capacity(MAX_ALTS);
    alts.extend(missing);
    alts.extend(wrong);
    alts.extend(extra);
    alts.extend(neighbours);
    alts.extend(muted);
    alts.truncate(MAX_ALTS);

    Some(ConfusionSet {
        chord: name.to_string(),
        ask,
        expected,
        alts,
        tuning: chart.tuning,
        capo: chart.capo,
    })
}
