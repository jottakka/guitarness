use guitarness_core::chart::{Chart, Dir, Event, EventKind, Loc, Span, Style, Voicing};
use guitarness_verify::hyp::{MAX_ALTS, confusion_set};
use guitarness_verify::types::{Ask, ConfusionSet, HypKind};

fn voicing(frets: [i8; 6], req: Option<&str>) -> Voicing {
    Voicing {
        frets,
        req: req.map(str::to_string),
        fingers: [0; 6],
        root: None,
    }
}

fn strum(chord: &str, dir: Dir, span: Option<Span>) -> Event {
    Event {
        tick: 0,
        dur: 480,
        kind: EventKind::Strum {
            dir,
            span,
            style: Style::Full,
            roll_ms: None,
        },
        chord: Some(chord.to_string()),
    }
}

fn chart_of(voicings: &[(&str, Voicing)], events: Vec<Event>) -> Chart {
    let mut c = Chart::new("t", 100.0);
    for (n, v) in voicings {
        c.voicings.insert((*n).to_string(), v.clone());
    }
    for (i, e) in events.into_iter().enumerate() {
        let mut e = e;
        e.tick = i as u32 * 480;
        c.tiers[0].events.push(e);
    }
    c
}

fn loc(i: usize) -> Loc {
    Loc { tier: 0, index: i }
}

/// The nine `guitarness-sim` chords (copied by hand, not read from the sim).
const NINE: [(&str, [i8; 6]); 9] = [
    ("C", [-1, 3, 2, 0, 1, 0]),
    ("D", [-1, -1, 0, 2, 3, 2]),
    ("E", [0, 2, 2, 1, 0, 0]),
    ("G", [3, 2, 0, 0, 0, 3]),
    ("A", [-1, 0, 2, 2, 2, 0]),
    ("Am", [-1, 0, 2, 2, 1, 0]),
    ("Em", [0, 2, 2, 0, 0, 0]),
    ("Dm", [-1, -1, 0, 2, 3, 1]),
    ("F", [-1, -1, 3, 2, 1, 1]),
];

/// Hand-counted tables: (chord, Missing, WrongFret, Extra). One `AllMuted` each, no neighbours.
/// WrongFret per Must string is {f-1, f+1, open} minus invalid and duplicates:
/// open -> 1, fret 1 -> 2, fret >= 2 -> 3.
const TABLE: [(&str, usize, usize, usize); 9] = [
    ("C", 5, 10, 1),
    ("D", 4, 10, 2),
    ("E", 6, 11, 0),
    ("G", 6, 12, 0),
    ("A", 5, 11, 1),
    ("Am", 5, 10, 1),
    ("Em", 6, 10, 0),
    ("Dm", 4, 9, 2),
    ("F", 4, 10, 2),
];

fn count(set: &ConfusionSet, f: impl Fn(&HypKind) -> bool) -> usize {
    set.alts.iter().filter(|h| f(&h.kind)).count()
}

fn single(name: &str, frets: [i8; 6], dir: Dir, span: Option<Span>, strict: bool) -> ConfusionSet {
    let c = chart_of(
        &[(name, voicing(frets, None))],
        vec![strum(name, dir, span)],
    );
    confusion_set(&c, loc(0), strict).expect("set")
}

#[test]
fn nine_chords_match_hand_tables() {
    for (name, frets) in NINE {
        let (_, m, w, x) = *TABLE.iter().find(|t| t.0 == name).unwrap();
        let s = single(name, frets, Dir::Down, None, false);
        assert_eq!(s.chord, name);
        assert_eq!(
            count(&s, |k| matches!(k, HypKind::Missing { .. })),
            m,
            "{name} Missing"
        );
        assert_eq!(
            count(&s, |k| matches!(k, HypKind::WrongFret { .. })),
            w,
            "{name} WrongFret"
        );
        assert_eq!(
            count(&s, |k| matches!(k, HypKind::Extra { .. })),
            x,
            "{name} Extra"
        );
        assert_eq!(count(&s, |k| matches!(k, HypKind::AllMuted)), 1, "{name}");
        assert_eq!(count(&s, |k| matches!(k, HypKind::Neighbour { .. })), 0);
        assert_eq!(s.alts.len(), m + w + x + 1, "{name} total");
        assert_eq!(s.expected.kind, HypKind::Expected);
        assert_eq!(s.expected.sounding, frets);
    }
}

#[test]
fn d_chord_details() {
    let s = single("D", [-1, -1, 0, 2, 3, 2], Dir::Down, None, false);
    assert_eq!(
        s.ask,
        [
            Ask::MustNot,
            Ask::MustNot,
            Ask::Must(0),
            Ask::Must(2),
            Ask::Must(3),
            Ask::Must(2)
        ]
    );
    // Missing(2): the open D string does not sound.
    let m2 = s
        .alts
        .iter()
        .find(|h| h.kind == HypKind::Missing { string: 2 })
        .unwrap();
    assert_eq!(m2.sounding, [-1, -1, -1, 2, 3, 2]);
    // Extra(0): the open low E rings.
    let x0 = s
        .alts
        .iter()
        .find(|h| h.kind == HypKind::Extra { string: 0 })
        .unwrap();
    assert_eq!(x0.sounding, [0, -1, 0, 2, 3, 2]);
    // WrongFret(4, f): 2, 4 and open.
    let mut f4: Vec<u8> = s
        .alts
        .iter()
        .filter_map(|h| match h.kind {
            HypKind::WrongFret { string: 4, fret } => Some(fret),
            _ => None,
        })
        .collect();
    f4.sort_unstable();
    assert_eq!(f4, vec![0, 2, 4]);
    let w = s
        .alts
        .iter()
        .find(|h| h.kind == HypKind::WrongFret { string: 4, fret: 4 })
        .unwrap();
    assert_eq!(w.sounding, [-1, -1, 0, 2, 4, 2]);
    // Open string: only fret+1 is a wrong fret.
    let f2: Vec<&HypKind> = s
        .alts
        .iter()
        .map(|h| &h.kind)
        .filter(|k| matches!(k, HypKind::WrongFret { string: 2, .. }))
        .collect();
    assert_eq!(f2, vec![&HypKind::WrongFret { string: 2, fret: 1 }]);
    assert_eq!(s.tuning, [40, 45, 50, 55, 59, 64]);
    assert_eq!(s.capo, 0);
}

#[test]
fn up_stroke_default_span_makes_low_strings_may() {
    // Default up span is 2..=5. E major: strings 0 and 1 are never reached.
    let s = single("E", [0, 2, 2, 1, 0, 0], Dir::Up, None, false);
    assert_eq!(s.ask[0], Ask::May);
    assert_eq!(s.ask[1], Ask::May);
    assert_eq!(s.ask[2], Ask::Must(2));
    assert_eq!(count(&s, |k| matches!(k, HypKind::Missing { .. })), 4);
    assert_eq!(s.expected.sounding, [-1, -1, 2, 1, 0, 0]);
    // D up: the two unplayed low strings are outside the reach, so no Extra at all.
    let d = single("D", [-1, -1, 0, 2, 3, 2], Dir::Up, None, true);
    assert_eq!(count(&d, |k| matches!(k, HypKind::Extra { .. })), 0);
    // Same chord, down: Extra(0) and Extra(1) exist.
    let dd = single("D", [-1, -1, 0, 2, 3, 2], Dir::Down, None, false);
    assert_eq!(count(&dd, |k| matches!(k, HypKind::Extra { .. })), 2);
}

#[test]
fn explicit_span_overrides_default() {
    let span = Some(Span { lo: 1, hi: 3 });
    let s = single("C", [-1, 3, 2, 0, 1, 0], Dir::Down, span, false);
    assert_eq!(
        s.ask,
        [
            Ask::May,
            Ask::Must(3),
            Ask::Must(2),
            Ask::Must(0),
            Ask::May,
            Ask::May
        ]
    );
    assert_eq!(s.expected.sounding, [-1, 3, 2, 0, -1, -1]);
}

#[test]
fn req_letters_and_strict() {
    // DESIGN 4.3 example D: "XxMMMM", frets [-1,0,0,2,3,2] (string 1 = open A, may be skipped).
    let v = voicing([-1, 0, 0, 2, 3, 2], Some("XxMMMM"));
    let c = chart_of(&[("D", v)], vec![strum("D", Dir::Down, None)]);
    let lax = confusion_set(&c, loc(0), false).unwrap();
    assert_eq!(lax.ask[0], Ask::MustNot);
    assert_eq!(lax.ask[1], Ask::May);
    assert_eq!(lax.expected.sounding, [-1, 0, 0, 2, 3, 2]);
    let strict = confusion_set(&c, loc(0), true).unwrap();
    assert_eq!(strict.ask[1], Ask::MustNot);
    assert_eq!(strict.expected.sounding, [-1, -1, 0, 2, 3, 2]);
    assert_eq!(
        count(&strict, |k| matches!(k, HypKind::Extra { .. })),
        count(&lax, |k| matches!(k, HypKind::Extra { .. })) + 1
    );
    // `m` stays May even when strict.
    let v2 = voicing([3, 2, 0, 0, 0, 3], Some("MMMMMm"));
    let c2 = chart_of(&[("G", v2)], vec![strum("G", Dir::Down, None)]);
    let g = confusion_set(&c2, loc(0), true).unwrap();
    assert_eq!(g.ask[5], Ask::May);
    assert_eq!(count(&g, |k| matches!(k, HypKind::Missing { .. })), 5);
}

#[test]
fn neighbours_are_previous_and_next_different_chord() {
    let vs: Vec<(&str, Voicing)> = NINE.iter().map(|(n, f)| (*n, voicing(*f, None))).collect();
    let c = chart_of(
        &vs,
        vec![
            strum("C", Dir::Down, None),
            strum("G", Dir::Down, None),
            strum("G", Dir::Up, None),
            strum("D", Dir::Down, None),
        ],
    );
    let s = confusion_set(&c, loc(2), false).unwrap();
    let names: Vec<&str> = s
        .alts
        .iter()
        .filter_map(|h| match &h.kind {
            HypKind::Neighbour { chord } => Some(chord.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["C", "D"]);
    // Up stroke reaches 2..=5 only: the neighbour sounds only there.
    let d = s
        .alts
        .iter()
        .find(|h| h.kind == HypKind::Neighbour { chord: "D".into() })
        .unwrap();
    assert_eq!(d.sounding, [-1, -1, 0, 2, 3, 2]);
    // First event has no previous chord; last has no next.
    let first = confusion_set(&c, loc(0), false).unwrap();
    assert_eq!(count(&first, |k| matches!(k, HypKind::Neighbour { .. })), 1);
    let last = confusion_set(&c, loc(3), false).unwrap();
    assert_eq!(count(&last, |k| matches!(k, HypKind::Neighbour { .. })), 1);
}

#[test]
fn neighbour_ignores_non_strum_events_and_unknown_voicings() {
    let mut mute = strum("C", Dir::Down, None);
    mute.kind = EventKind::Mute { dir: Dir::Down };
    let c = chart_of(
        &[
            ("G", voicing([3, 2, 0, 0, 0, 3], None)),
            ("C", voicing([-1, 3, 2, 0, 1, 0], None)),
        ],
        vec![
            mute,
            strum("G", Dir::Down, None),
            strum("Zz", Dir::Down, None),
        ],
    );
    let s = confusion_set(&c, loc(1), false).unwrap();
    // The mute is not a neighbour; "Zz" has no voicing so it is skipped.
    assert_eq!(count(&s, |k| matches!(k, HypKind::Neighbour { .. })), 0);
    assert!(confusion_set(&c, loc(2), false).is_none());
}

#[test]
fn no_duplicate_soundings_and_ceiling() {
    for (name, frets) in NINE {
        let s = single(name, frets, Dir::Down, None, false);
        let mut all: Vec<[i8; 6]> = s.alts.iter().map(|h| h.sounding).collect();
        all.push(s.expected.sounding);
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n, "{name} duplicates");
        assert!(s.alts.len() <= MAX_ALTS);
    }
    // Single Must string: Missing(s) and AllMuted both sound nothing; keep the first only.
    let one = single("X", [-1, -1, -1, -1, -1, 2], Dir::Down, None, false);
    assert_eq!(count(&one, |k| matches!(k, HypKind::AllMuted)), 0);
    assert_eq!(count(&one, |k| matches!(k, HypKind::Missing { .. })), 1);
    // Six Must strings at mid frets: 6 + 18 + 1 = 25 > 24 before the ceiling.
    let big = chart_of(
        &[
            ("B", voicing([2, 4, 4, 3, 2, 2], None)),
            ("N1", voicing([1, 1, 1, 1, 1, 1], None)),
            ("N2", voicing([5, 5, 5, 5, 5, 5], None)),
        ],
        vec![
            strum("N1", Dir::Down, None),
            strum("B", Dir::Down, None),
            strum("N2", Dir::Down, None),
        ],
    );
    let s = confusion_set(&big, loc(1), false).unwrap();
    assert_eq!(s.alts.len(), MAX_ALTS);
    // WrongFret is what is dropped: everything else survives.
    assert_eq!(count(&s, |k| matches!(k, HypKind::Missing { .. })), 6);
    assert_eq!(count(&s, |k| matches!(k, HypKind::Neighbour { .. })), 2);
    assert_eq!(count(&s, |k| matches!(k, HypKind::AllMuted)), 1);
    assert_eq!(count(&s, |k| matches!(k, HypKind::WrongFret { .. })), 15);
}

#[test]
fn non_strums_and_bad_locs_give_none() {
    let v = voicing([-1, 3, 2, 0, 1, 0], None);
    let mut mute = strum("C", Dir::Down, None);
    mute.kind = EventKind::Mute { dir: Dir::Down };
    let mut ghost = strum("C", Dir::Down, None);
    ghost.kind = EventKind::Ghost { dir: Dir::Up };
    let mut rest = strum("C", Dir::Down, None);
    rest.kind = EventKind::Rest;
    let mut note = strum("C", Dir::Down, None);
    note.kind = EventKind::Note {
        string: 1,
        fret: 3,
        finger: None,
    };
    let c = chart_of(
        &[("C", v)],
        vec![mute, ghost, rest, note, strum("C", Dir::Down, None)],
    );
    for i in 0..4 {
        assert!(confusion_set(&c, loc(i), false).is_none(), "event {i}");
    }
    assert!(confusion_set(&c, loc(4), false).is_some());
    assert!(confusion_set(&c, loc(9), false).is_none());
    assert!(confusion_set(&c, Loc { tier: 5, index: 0 }, false).is_none());
    // Unknown voicing and a strum that requires nothing.
    let c2 = chart_of(
        &[("Z", voicing([-1; 6], None))],
        vec![strum("Q", Dir::Down, None), strum("Z", Dir::Down, None)],
    );
    assert!(confusion_set(&c2, loc(0), false).is_none());
    assert!(confusion_set(&c2, loc(1), false).is_none());
}

#[test]
fn capo_and_tuning_are_copied() {
    let mut c = chart_of(
        &[("C", voicing([-1, 3, 2, 0, 1, 0], None))],
        vec![strum("C", Dir::Down, None)],
    );
    c.capo = 2;
    c.tuning = [38, 45, 50, 55, 59, 64];
    let s = confusion_set(&c, loc(0), false).unwrap();
    assert_eq!(s.capo, 2);
    assert_eq!(s.tuning[0], 38);
}

#[test]
fn deterministic() {
    let a = single("G", [3, 2, 0, 0, 0, 3], Dir::Down, None, false);
    let b = single("G", [3, 2, 0, 0, 0, 3], Dir::Down, None, false);
    assert_eq!(a, b);
}
