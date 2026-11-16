//! Golden parity against the JavaScript demo (legacy/web-demo/test/golden).
//!
//! Declared tolerances (per frame, against the CSV, which is itself rounded):
//! - t: 2e-6 s · f: 5e-3 Hz (when both non-zero; zero/non-zero must agree)
//! - c: 2e-4 · rms, rmsS: 1e-5 relative + 1e-9 · presence: 2e-4
//!
//! Events: same sequence of (kind, midi, via); |Δt| ≤ 1e-5 s (JSON rounds to 5 decimals).
//! Direct autocorrelation must meet all of this. The FFT back-end must produce the same
//! events; its per-frame deviations are reported, not asserted (today they also pass).

use std::path::{Path, PathBuf};
use tonica_dsp::{AcfMode, Event, Frame, Pipeline, Via};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../legacy/web-demo/test/golden")
}

const CASES: [&str; 5] = [
    "A-isoladas",
    "B-soando",
    "C-alvo-rapido",
    "D-forte-fraco",
    "F-oitava-errada",
];

struct CsvFrame {
    t: f64,
    f: f64,
    c: f64,
    rms: f64,
    rms_s: f64,
    tid: u32,
    p: Vec<f64>,
}

fn read_csv(path: &Path) -> Vec<CsvFrame> {
    let s = std::fs::read_to_string(path).unwrap();
    s.lines()
        .skip(1)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split(',').collect();
            CsvFrame {
                t: c[0].parse().unwrap(),
                f: c[1].parse().unwrap(),
                c: c[2].parse().unwrap(),
                rms: c[3].parse().unwrap(),
                rms_s: c[4].parse().unwrap(),
                tid: c[5].parse().unwrap(),
                p: if c[6].is_empty() {
                    vec![]
                } else {
                    c[6].split(';').map(|v| v.parse().unwrap()).collect()
                },
            }
        })
        .collect()
}

fn read_wav(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).unwrap();
    assert_eq!(r.spec().sample_rate, 48000);
    r.samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect()
}

#[derive(Debug, PartialEq, Clone)]
struct Ev {
    t: f64,
    kind: String,
    midi: Option<i64>,
    via: Option<String>,
}

fn expected_events(json: &serde_json::Value) -> Vec<Ev> {
    json["eventos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| Ev {
            t: e["t"].as_f64().unwrap(),
            kind: e["tipo"].as_str().unwrap().to_string(),
            midi: e["midi"].as_i64(),
            via: e["via"].as_str().map(|s| s.to_string()),
        })
        .collect()
}

struct Run {
    frames: Vec<Frame>,
    events: Vec<Ev>,
}

fn run_case(name: &str, mode: AcfMode) -> (Run, serde_json::Value) {
    let dir = golden_dir();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{name}.json"))).unwrap())
            .unwrap();
    let x = read_wav(&dir.join(format!("{name}.wav")));
    let sr = 48000.0;
    let mut pipe = Pipeline::demo(sr, mode, 128);
    let alvos: Vec<(f64, Vec<i32>)> = json["alvos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["t"].as_f64().unwrap(),
                a["midis"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| m.as_i64().unwrap() as i32)
                    .collect(),
            )
        })
        .collect();
    let mut si = 0;
    let mut frames = Vec::new();
    let mut events = Vec::new();
    let mut b = 0usize;
    while (b + 1) * 128 <= x.len() {
        let now = (b * 128) as f64 / sr;
        while si < alvos.len() && alvos[si].0 <= now {
            pipe.set_targets(&alvos[si].1, now);
            si += 1;
        }
        pipe.process(
            &x[b * 128..(b + 1) * 128],
            now,
            |fr| frames.push(*fr),
            |e| {
                events.push(match e {
                    Event::Attack { t, .. } => Ev {
                        t,
                        kind: "ataque".into(),
                        midi: None,
                        via: None,
                    },
                    Event::Note { t, midi, via, .. } => Ev {
                        t,
                        kind: "nota".into(),
                        midi: Some(midi as i64),
                        via: Some(match via {
                            Via::Target => "alvo".into(),
                            Via::Mpm => "mpm".into(),
                        }),
                    },
                })
            },
        );
        b += 1;
    }
    (Run { frames, events }, json)
}

#[derive(Default, Debug)]
struct Dev {
    f: f64,
    c: f64,
    rms: f64,
    p: f64,
    t: f64,
    zero_flip: usize,
    bad: usize,
}

fn compare_frames(name: &str, got: &[Frame], csv: &[CsvFrame]) -> Dev {
    assert_eq!(got.len(), csv.len(), "{name}: frame count");
    let mut d = Dev::default();
    for (i, (g, e)) in got.iter().zip(csv).enumerate() {
        let mut bad = false;
        d.t = d.t.max((g.t - e.t).abs());
        bad |= (g.t - e.t).abs() > 2e-6;
        if (g.f > 0.0) != (e.f > 0.0) {
            d.zero_flip += 1;
            bad = true;
        } else {
            d.f = d.f.max((g.f - e.f).abs());
            bad |= (g.f - e.f).abs() > 5e-3;
        }
        d.c = d.c.max((g.c - e.c).abs());
        bad |= (g.c - e.c).abs() > 2e-4;
        for (a, b) in [(g.rms, e.rms), (g.rms_s, e.rms_s)] {
            let r = (a - b).abs() / (b.abs() + 1e-9);
            d.rms = d.rms.max(r);
            bad |= (a - b).abs() > 1e-5 * b.abs() + 1e-9;
        }
        assert_eq!(g.tid, e.tid, "{name}: tid at frame {i}");
        assert_eq!(
            g.presences().len(),
            e.p.len(),
            "{name}: presence count at frame {i}"
        );
        for (a, b) in g.presences().iter().zip(&e.p) {
            d.p = d.p.max((a - b).abs());
            bad |= (a - b).abs() > 2e-4;
        }
        if bad {
            d.bad += 1;
            if d.bad <= 3 {
                eprintln!(
                    "{name} frame {i}: got t={:.6} f={:.4} c={:.5} rms={:.6e} p={:?} | csv t={:.6} f={:.4} c={:.5} rms={:.6e} p={:?}",
                    g.t,
                    g.f,
                    g.c,
                    g.rms,
                    g.presences(),
                    e.t,
                    e.f,
                    e.c,
                    e.rms,
                    e.p
                );
            }
        }
    }
    d
}

fn compare_events(name: &str, got: &[Ev], exp: &[Ev]) -> Result<(), String> {
    if got.len() != exp.len() {
        return Err(format!(
            "{name}: {} events, expected {}\n got {:?}\n exp {:?}",
            got.len(),
            exp.len(),
            got,
            exp
        ));
    }
    for (i, (g, e)) in got.iter().zip(exp).enumerate() {
        if g.kind != e.kind || g.midi != e.midi || g.via != e.via || (g.t - e.t).abs() > 1e-5 {
            return Err(format!(
                "{name}: event {i} differs: got {g:?}, expected {e:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn golden_direct_parity() {
    let mut fails = Vec::new();
    for name in CASES {
        let (run, json) = run_case(name, AcfMode::Direct);
        let csv = read_csv(&golden_dir().join(format!("{name}.quadros.csv")));
        let d = compare_frames(name, &run.frames, &csv);
        let ev = compare_events(name, &run.events, &expected_events(&json));
        println!(
            "direct {name:16} frames {:5} bad {:3} flips {} | max Δf {:.2e} Δc {:.2e} Δrms(rel) {:.2e} Δp {:.2e} Δt {:.1e} | events {}",
            run.frames.len(),
            d.bad,
            d.zero_flip,
            d.f,
            d.c,
            d.rms,
            d.p,
            d.t,
            if ev.is_ok() { "OK" } else { "DIFF" }
        );
        if d.bad > 0 {
            fails.push(format!("{name}: {} frames outside tolerance", d.bad));
        }
        if let Err(e) = ev {
            fails.push(e);
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

#[test]
fn golden_fft_events() {
    let mut fails = Vec::new();
    for name in CASES {
        let (run, json) = run_case(name, AcfMode::Fft);
        let csv = read_csv(&golden_dir().join(format!("{name}.quadros.csv")));
        let d = compare_frames(name, &run.frames, &csv);
        let ev = compare_events(name, &run.events, &expected_events(&json));
        println!(
            "fft    {name:16} frames {:5} bad {:3} flips {} | max Δf {:.2e} Δc {:.2e} Δp {:.2e} | events {}",
            run.frames.len(),
            d.bad,
            d.zero_flip,
            d.f,
            d.c,
            d.p,
            if ev.is_ok() { "OK" } else { "DIFF" }
        );
        if let Err(e) = ev {
            fails.push(e);
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// Scoring like test/golden.js: a played note counts if a note event with its MIDI number
/// arrives between its onset (−5 ms) and the next onset.
#[test]
fn golden_scores_match_resumo() {
    let expected = [
        ("A-isoladas", 37, 37),
        ("B-soando", 23, 23),
        ("C-alvo-rapido", 22, 23),
        ("D-forte-fraco", 20, 23),
        ("F-oitava-errada", 9, 9),
    ];
    for (name, ok_exp, total) in expected {
        let (run, json) = run_case(name, AcfMode::Fft);
        let pl = json["tocadas"].as_array().unwrap();
        let notes: Vec<&Ev> = run.events.iter().filter(|e| e.kind == "nota").collect();
        let mut ok = 0;
        for (k, p) in pl.iter().enumerate() {
            let t = p["t"].as_f64().unwrap();
            let fim = pl
                .get(k + 1)
                .map(|q| q["t"].as_f64().unwrap())
                .unwrap_or(t + 0.6);
            let m = p["midi"].as_i64().unwrap();
            if notes
                .iter()
                .any(|e| e.t >= t - 0.005 && e.t < fim && e.midi == Some(m))
            {
                ok += 1;
            }
        }
        println!("score  {name:16} {ok}/{total}");
        assert_eq!((ok, pl.len()), (ok_exp, total), "{name}");
        if name == "F-oitava-errada" {
            assert!(
                notes.iter().all(|e| e.via.as_deref() != Some("alvo")),
                "F: no event may come via target"
            );
        }
    }
}
