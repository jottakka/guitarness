use tonica_core::chart::*;
use tonica_core::pulse::{Exercise, exercise, expectations};
use tonica_core::timeline::{self, Kind, Style as TStyle, Timeline, demo_chart, gallery_chart};
use tonica_theory::interval::Interval;

const DESIGN_EXAMPLE: &str = r#"
{ "v": 1, "id": "x", "title": "T", "origin": "paste",
  "tuning": [40,45,50,55,59,64], "capo": 0, "tpq": 480,
  "tempo_map": [{ "tick": 0, "bpm": 92 }], "time_sigs": [{ "tick": 0, "num": 4, "den": 4 }],
  "audio": null,
  "voicings": { "D": { "frets": [-1,0,0,2,3,2], "req": "XxMMMM", "fingers": [0,0,0,1,3,2] } },
  "patterns": { "pop8": { "grid": 8, "cells": ["D","-","D","U","-","U","D","U"] } },
  "sections": [{ "id": "verso1", "name": "Verso", "start": 0, "end": 15360 }],
  "tiers": [{ "tier": 2, "name": "Batida", "events": [
    { "tick": 0,   "dur": 240, "kind": "strum", "chord": "D", "dir": "D", "span": [2,5] },
    { "tick": 240, "dur": 240, "kind": "strum", "chord": "D", "dir": "U", "span": [3,5] },
    { "tick": 480, "dur": 240, "kind": "mute",  "dir": "D" },
    { "tick": 720, "dur": 480, "kind": "note",  "string": 2, "fret": 0, "finger": "p" },
    { "tick": 1200,"dur": 480, "kind": "rest" } ] }],
  "lyrics": [], "future_field": 7 }"#;

const A2_EXAMPLE: &str = r#"
{ "v": 1, "tpq": 480,
  "tempo_map": [{ "tick": 0, "bpm": 100 }], "time_sigs": [{ "tick": 0, "num": 4, "den": 4 }],
  "voicings": { "G": { "frets": [3,2,0,0,0,3] }, "C": { "frets": [-1,3,2,0,1,0] }, "E": { "frets": [0,2,2,1,0,0] } },
  "tiers": [{ "tier": 2, "name": "Golpes", "events": [
    { "tick": 0,    "dur": 240, "kind": "strum", "chord": "G", "dir": "D", "span": [0,5] },
    { "tick": 240,  "dur": 240, "kind": "strum", "chord": "G", "dir": "U", "span": [3,5], "style": "accent" },
    { "tick": 480,  "dur": 240, "kind": "mute",  "dir": "D" },
    { "tick": 720,  "dur": 240, "kind": "strum", "chord": "G", "dir": "D", "span": [0,2] },
    { "tick": 960,  "dur": 480, "kind": "strum", "chord": "C", "dir": "D", "style": "roll", "roll_ms": 150 },
    { "tick": 1440, "dur": 240, "kind": "ghost", "dir": "U" },
    { "tick": 1680, "dur": 240, "kind": "strum", "chord": "E", "dir": "D", "style": "pm" },
    { "tick": 1920, "dur": 240, "kind": "note",  "string": 0, "fret": 3, "finger": "p" } ] }] }"#;

fn roundtrip(src: &str) -> Chart {
    let a = Chart::from_json(src).expect("parse");
    let j = a.to_json();
    let b = Chart::from_json(&j).expect("reparse");
    assert_eq!(a, b);
    assert_eq!(j, b.to_json(), "to_json is stable");
    assert!(!j.contains('\n'));
    a
}

#[test]
fn json_roundtrip_design_example() {
    let c = roundtrip(DESIGN_EXAMPLE);
    assert_eq!(c.events().len(), 5);
    assert_eq!(c.tempo_map[0].bpm, 92.0);
    assert_eq!(c.voicings["D"].req.as_deref(), Some("XxMMMM"));
    assert_eq!(c.patterns["pop8"].cells.len(), 8);
    // Unknown fields are ignored.
    assert!(!c.to_json().contains("future_field"));
    let v: serde_json::Value = serde_json::from_str(&c.to_json()).unwrap();
    assert_eq!(v["tempo_map"][0]["bpm"], 92);
    assert_eq!(
        v["tiers"][0]["events"][0]["span"],
        serde_json::json!([2, 5])
    );
}

#[test]
fn json_roundtrip_a2_example() {
    let c = roundtrip(A2_EXAMPLE);
    assert_eq!(c.events().len(), 8);
    assert!(matches!(
        c.events()[1].kind,
        EventKind::Strum {
            dir: Dir::Up,
            style: Style::Accent,
            span: Some(Span { lo: 3, hi: 5 }),
            ..
        }
    ));
    assert!(matches!(
        c.events()[4].kind,
        EventKind::Strum {
            style: Style::Roll,
            roll_ms: Some(150),
            ..
        }
    ));
    assert!(matches!(
        c.events()[5].kind,
        EventKind::Ghost { dir: Dir::Up }
    ));
    assert!(matches!(
        c.events()[6].kind,
        EventKind::Strum {
            style: Style::Pm,
            ..
        }
    ));
    assert!(matches!(
        c.events()[7].kind,
        EventKind::Note {
            string: 0,
            fret: 3,
            finger: Some('p')
        }
    ));
}

#[test]
fn builtin_charts_roundtrip_and_validate() {
    for c in [
        demo_chart(),
        gallery_chart(),
        exercise(Exercise::OnClickEighths, 80.0, 2),
    ] {
        c.validate().unwrap();
        let back = Chart::from_json(&c.to_json()).unwrap();
        assert_eq!(c, back);
        Chart::from_json(&c.to_json_pretty()).unwrap();
    }
}

fn with(events: &str) -> String {
    format!(
        r#"{{"v":1,"tpq":480,"tempo_map":[{{"tick":0,"bpm":90}}],"time_sigs":[{{"tick":0,"num":4,"den":4}}],
        "voicings":{{"D":{{"frets":[-1,-1,0,2,3,2]}}}},
        "tiers":[{{"tier":2,"name":"t","events":[{events}]}}]}}"#
    )
}

#[test]
fn eight_invalid_charts_each_with_the_right_error() {
    let ok = r#"{"tick":0,"dur":240,"kind":"strum","chord":"D","dir":"D"}"#;
    Chart::from_json(&with(ok)).unwrap();
    let loc = |index| Loc { tier: 0, index };

    let e = |s: &str| Chart::from_json(s).unwrap_err();
    assert!(matches!(e("{not json"), ChartError::Json(_)));
    assert_eq!(
        e(&with(ok).replace("\"v\":1", "\"v\":2")),
        ChartError::BadVersion(2)
    );
    assert_eq!(
        e(&with(&format!(
            "{},{}",
            r#"{"tick":480,"dur":240,"kind":"mute"}"#, r#"{"tick":240,"dur":240,"kind":"mute"}"#
        ))),
        ChartError::TicksOutOfOrder(loc(1))
    );
    assert_eq!(
        e(&with(r#"{"tick":0,"dur":0,"kind":"mute"}"#)),
        ChartError::ZeroDuration(loc(0))
    );
    assert_eq!(
        e(&with(
            r#"{"tick":0,"dur":240,"kind":"strum","chord":"D","span":[2,6]}"#
        )),
        ChartError::SpanOutOfRange(loc(0))
    );
    assert_eq!(
        e(&with(
            r#"{"tick":0,"dur":240,"kind":"strum","chord":"D","span":[4,2]}"#
        )),
        ChartError::SpanReversed(loc(0))
    );
    assert_eq!(
        e(&with(r#"{"tick":0,"dur":240,"kind":"strum","chord":"Zz"}"#)),
        ChartError::UnknownChord(loc(0), "Zz".into())
    );
    assert_eq!(
        e(&with(r#"{"tick":0,"dur":240,"kind":"slap"}"#)),
        ChartError::UnknownKind(loc(0), "slap".into())
    );
    assert_eq!(
        e(&with(ok).replace("[{\"tick\":0,\"bpm\":90}]", "[]")),
        ChartError::EmptyTempoMap
    );
    assert_eq!(
        e(&with(ok).replace("\"tpq\":480,", "")),
        ChartError::MissingField("tpq".into())
    );
    assert!(matches!(
        e(&with(r#"{"tick":0,"dur":240,"kind":"mute","dir":"X"}"#)),
        ChartError::BadValue(_, _)
    ));
    assert!(matches!(
        e(&with(
            r#"{"tick":0,"dur":240,"kind":"note","string":6,"fret":0}"#
        )),
        ChartError::BadValue(_, _)
    ));
}

#[test]
fn seconds_of_tick_with_tempo_change() {
    let mut c = Chart::new("t", 120.0);
    c.tempo_map.push(TempoPoint {
        tick: 1920,
        bpm: 60.0,
    });
    // 4 beats at 120 bpm = 2 s.
    assert!((c.seconds_of_tick(1920) - 2.0).abs() < 1e-12);
    assert!((c.seconds_of_tick(960) - 1.0).abs() < 1e-12);
    // Then one beat at 60 bpm = 1 s.
    assert!((c.seconds_of_tick(1920 + 480) - 3.0).abs() < 1e-12);
    assert!((c.seconds_of_tick(1920 + 240) - 2.5).abs() < 1e-12);
    for t in [0, 1, 960, 1920, 2400, 5000] {
        assert_eq!(c.tick_of_seconds(c.seconds_of_tick(t)), t);
    }
    assert_eq!(c.bpm_at(1919), 120.0);
    assert_eq!(c.bpm_at(1920), 60.0);
}

#[test]
fn bar_positions_follow_time_signatures() {
    let mut c = Chart::new("t", 100.0);
    c.time_sigs.push(TimeSig {
        tick: 1920 * 2,
        num: 3,
        den: 4,
    });
    assert_eq!(c.bar_pos(0), (0, 0));
    assert_eq!(c.bar_pos(1920 + 240), (1, 240));
    assert_eq!(c.bar_pos(1920 * 2), (2, 0));
    assert_eq!(c.bar_pos(1920 * 2 + 1440), (3, 0));
    assert_eq!(c.eighth_in_bar(1920 + 720), 3);
    assert_eq!(root_pc_of_name("F#m7"), Some(6));
    assert_eq!(root_pc_of_name("Bb"), Some(10));
    assert_eq!(root_pc_of_name("x"), None);
}

#[test]
fn timeline_reproduces_demo() {
    let t = Timeline::demo();
    assert_eq!(t.name, "demo");
    assert_eq!(t.events.len(), 4 * 6 + 6);
    assert!((t.length - 12.0).abs() < 1e-4);
    assert!((t.beat - 0.75).abs() < 1e-6);
    assert_eq!(t.speed, 1.0);
    let names: Vec<&str> = t.chords.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["G", "C", "D", "Em"]);
    // Times in order; bar 0: D at 0, D at 2 eighths, U, U, D, U.
    let eighth = 0.375f32;
    assert!(t.events.windows(2).all(|w| w[0].t <= w[1].t));
    let bar0: Vec<f32> = t
        .events
        .iter()
        .take_while(|e| e.chord == 0)
        .map(|e| e.t / eighth)
        .collect();
    assert_eq!(bar0, [0.0, 2.0, 3.0, 5.0, 6.0, 7.0]);
    // Labels sit on the first event of each bar only.
    let labels: Vec<(f32, &str)> = t
        .events
        .iter()
        .filter_map(|e| e.label.as_deref().map(|l| (e.t, l)))
        .collect();
    assert_eq!(labels.len(), 4);
    assert_eq!(labels[1].1, "C");
    assert!((labels[1].0 - 3.0).abs() < 1e-4);
    // Notes: 0.6 s sustain, single glyph; bar 1 first note string 0 fret 3 on C = 3 semis over E... midi 43, root C -> 7 = fifth.
    let notes: Vec<_> = t
        .events
        .iter()
        .filter(|e| matches!(e.kind, Kind::Note { .. }))
        .collect();
    assert_eq!(notes.len(), 6);
    assert!((notes[0].dur - 0.6).abs() < 1e-3 || (notes[0].dur - 0.6).abs() < 0.01);
    let g = t.glyphs(notes[0]);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].interval, Interval::Fifth);
    // G chord down strum: six glyphs, low to high; root on string 0 (fret 3 = G).
    let first = &t.events[0];
    let g = t.glyphs(first);
    assert_eq!(
        g.iter().map(|x| x.string).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, 5]
    );
    assert_eq!(g[0].interval, Interval::Root);
    assert_eq!(g[1].interval, Interval::MajorThird); // B
    // Up strokes skip the two lowest strings, listed high to low.
    let up = t
        .events
        .iter()
        .find(|e| matches!(e.kind, Kind::Strum { dir: Dir::Up, .. }))
        .unwrap();
    let g = t.glyphs(up);
    assert_eq!(g.iter().map(|x| x.string).collect::<Vec<_>>(), [5, 4, 3, 2]);
    assert!(g.iter().all(|x| x.dt == 0.0));
    assert!(Timeline::judged(up));
}

#[test]
fn timeline_reproduces_gallery() {
    let t = Timeline::gallery();
    assert_eq!(t.name, "strokes");
    assert_eq!(t.speed, 0.55);
    assert_eq!(t.events.len(), 15 + 4);
    assert!((t.length - 24.0 * 60.0 / 70.0).abs() < 1e-3);
    let beat = 60.0 / 70.0;
    let style = |k: usize| match t.events[k].kind {
        Kind::Strum { style, .. } => style,
        _ => panic!("note"),
    };
    assert_eq!(style(0), TStyle::Normal);
    assert_eq!(style(2), TStyle::Mute);
    assert_eq!(style(4), TStyle::Accent);
    assert_eq!(style(6), TStyle::Bass);
    assert_eq!(style(7), TStyle::Treble);
    assert_eq!(style(8), TStyle::Roll);
    assert_eq!(style(11), TStyle::Ghost);
    assert_eq!(style(12), TStyle::PalmMute);
    assert!((t.events[12].dur - 0.2143).abs() < 0.01 && t.events[11].dur == 0.0);
    // Group starts: three beats apart; palm mutes at 0, .75, 1.5 beats.
    assert!((t.events[6].t - 9.0 * beat as f32).abs() < 1e-3);
    assert!((t.events[13].t - (18.0 + 0.75) * beat as f32).abs() < 1e-3);
    // Bass covers strings 0..=2 of G, treble 3..=5.
    assert_eq!(t.span(&t.events[6]), Some((0, 2)));
    assert_eq!(t.span(&t.events[7]), Some((3, 5)));
    // Chuck: pitch-free, covers the chord's played strings; ghost never judged.
    assert_eq!(t.expected(&t.events[2]), [false; 6]);
    assert_eq!(
        t.covered(&t.events[2]),
        [false, true, true, true, true, true]
    );
    assert!(!Timeline::judged(&t.events[11]));
    // Roll up: high strings first, 90 ms steps.
    let g = t.glyphs(&t.events[9]);
    assert_eq!(g.iter().map(|x| x.string).collect::<Vec<_>>(), [5, 4, 3, 2]);
    assert!((g[2].dt - 0.18).abs() < 1e-6);
    // Fingerpicking keeps its fingers.
    let fingers: Vec<_> = t
        .events
        .iter()
        .filter_map(|e| match e.kind {
            Kind::Note { finger, .. } => finger,
            _ => None,
        })
        .collect();
    assert_eq!(fingers, ['p', 'i', 'm', 'a']);
    // Chord label on the first event of each group.
    assert_eq!(t.events.iter().filter(|e| e.label.is_some()).count(), 8);
    let _ = timeline::OPEN;
}

#[test]
fn roll_ms_sets_total_spread() {
    let c = Chart::from_json(A2_EXAMPLE).unwrap();
    let t = Timeline::from_chart(&c);
    let roll = t
        .events
        .iter()
        .find(|e| {
            matches!(
                e.kind,
                Kind::Strum {
                    style: TStyle::Roll,
                    ..
                }
            )
        })
        .unwrap();
    let g = t.glyphs(roll); // C: five strings
    assert_eq!(g.len(), 5);
    assert!((g[4].dt - 0.150).abs() < 1e-6);
}

#[test]
fn exercises_at_60_80_100_bpm() {
    for bpm in [60.0, 80.0, 100.0] {
        let q = exercise(Exercise::OnClickQuarters, bpm, 2);
        q.validate().unwrap();
        let ticks: Vec<u32> = q.events().iter().map(|e| e.tick).collect();
        assert_eq!(ticks, [0, 480, 960, 1440, 1920, 2400, 2880, 3360]);
        assert!(
            q.events()
                .iter()
                .all(|e| e.kind == EventKind::Mute { dir: Dir::Down } && e.dur == 480)
        );
        let beat = 60.0 / bpm;
        assert!((q.seconds_of_tick(480) - beat).abs() < 1e-12);
        assert!((q.seconds_of_tick(3840) - 8.0 * beat).abs() < 1e-9);

        let e = exercise(Exercise::OnClickEighths, bpm, 1);
        let ticks: Vec<u32> = e.events().iter().map(|e| e.tick).collect();
        assert_eq!(ticks, [0, 240, 480, 720, 960, 1200, 1440, 1680]);
        for (k, ev) in e.events().iter().enumerate() {
            let want = if k % 2 == 0 { Dir::Down } else { Dir::Up };
            assert_eq!(ev.kind, EventKind::Mute { dir: want });
        }
        let ex = expectations(&e);
        assert_eq!(ex.len(), 8);
        assert_eq!(ex[3].pos_in_bar, 3);
        assert!((ex[3].t_s - 3.0 * beat / 2.0).abs() < 1e-12);
        assert!((ex[3].prev_gap_s - beat / 2.0).abs() < 1e-12);
        assert_eq!(ex[0].prev_gap_s, ex[0].next_gap_s);
        assert!(ex.iter().all(|x| !x.ghost));
    }
    // Out-of-range input is clamped, never invalid.
    exercise(Exercise::OnClickQuarters, 0.0, 0)
        .validate()
        .unwrap();
    exercise(Exercise::OnClickQuarters, f64::NAN, 3)
        .validate()
        .unwrap();
}

#[test]
fn expectations_skip_rests_and_flag_ghosts() {
    let c = Chart::from_json(A2_EXAMPLE).unwrap();
    let ex = expectations(&c);
    assert_eq!(ex.len(), 8);
    assert_eq!(ex.iter().filter(|x| x.ghost).count(), 1);
    assert_eq!(
        ex.iter().map(|x| x.id).collect::<Vec<_>>(),
        (0..8).collect::<Vec<_>>()
    );
    let d = Chart::from_json(DESIGN_EXAMPLE).unwrap();
    assert_eq!(expectations(&d).len(), 4); // the rest is not an expectation
}
