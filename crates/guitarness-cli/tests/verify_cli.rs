//! `guitarness verify` on bench tapes written here: chord strums rendered by `guitarness-sim`, taped
//! with the real writer. No device, no microphone, no window.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use guitarness_audio::capture::{CaptureRings, CaptureStage};
use guitarness_audio::{ChannelPick, EngineEvent, OnsetNote, Stamp};
use guitarness_core::chart::{Chart, Dir as CDir, Event, EventKind, Style, Voicing};
use guitarness_core::verdict::{Attack, Direction, JudgeOut, PulseVerdict, Timing, Why};
use guitarness_dsp::synth::Mulberry32;
use guitarness_dsp::{OnsetEvent, Strum};
use guitarness_session::format::{TapeEvent, TapeHeader};
use guitarness_session::writer::TapeWriter;
use guitarness_sim::strum::{Dir, StrumSpec, Voicing as SimVoicing, render_take, voicing};
use serde_json::Value;

const FS: u32 = 48_000;
const SPREAD_S: f64 = 0.02;

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "guitarness-v5-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn guitarness(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_guitarness"))
        .args(args)
        .output()
        .expect("run guitarness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json(args: &[&str]) -> (i32, Value) {
    let (code, out, err) = guitarness(args);
    let v =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("bad json ({e}): {out}\n{err}"));
    (code, v)
}

/// A chart of down-strums of G, one per second (60 bpm), `n` of them.
fn g_chart(n: u32) -> Chart {
    let mut c = Chart::new("verify", 60.0);
    c.voicings.insert(
        "G".to_string(),
        Voicing {
            frets: [3, 2, 0, 0, 0, 3],
            req: None,
            fingers: [0; 6],
            root: Some(7),
        },
    );
    for i in 0..n {
        c.tiers[0].events.push(Event {
            tick: i * c.tpq,
            dur: c.tpq,
            kind: EventKind::Strum {
                dir: CDir::Down,
                span: None,
                style: Style::Full,
                roll_ms: None,
            },
            chord: Some("G".to_string()),
        });
    }
    c
}

fn chunk_stamp(first: u64, frames: usize) -> Stamp {
    Stamp {
        first,
        frames: frames as u32,
        device_ns: 0,
        callback_ns: 0,
    }
}

/// Writes a tape of `played.len()` strums of the chart's G, one per second from 0.5 s; strum
/// `i` is played with the voicing `played[i]`. Every strum is heard and judged `Center`.
/// `chart_strums` is how many strums the chart has (`None`: as many as played).
fn make_tape(
    root: &Path,
    played: &[SimVoicing],
    label: &str,
    chart_strums: Option<u32>,
) -> PathBuf {
    make_tape_with(root, played, label, chart_strums, 0.0)
}

/// As [`make_tape`], with the whole recording played back `cents` sharp (a detuned guitar).
fn make_tape_with(
    root: &Path,
    played: &[SimVoicing],
    label: &str,
    chart_strums: Option<u32>,
    cents: f64,
) -> PathBuf {
    let chart = g_chart(chart_strums.unwrap_or(played.len() as u32));
    let specs: Vec<StrumSpec> = played
        .iter()
        .enumerate()
        .map(|(i, v)| StrumSpec {
            t0: 0.5 + i as f64,
            voicing: *v,
            dir: Dir::Down,
            spread: SPREAD_S,
            gain: 1.0,
            muted: false,
            strings: [true; 6],
            attack: 0.001,
        })
        .collect();
    let len = ((played.len() as f64 + 1.0) * f64::from(FS)) as usize;
    let mut rng = Mulberry32::new(11);
    let mut audio = render_take(f64::from(FS), &specs, len, &mut rng).audio;
    if cents != 0.0 {
        // Linear-interpolation resampling: pitch and time move together; onsets stay put
        // in the taped events, so the strum times are scaled the same way below.
        let k = 2f64.powf(cents / 1200.0);
        let src = audio.clone();
        for (i, o) in audio.iter_mut().enumerate() {
            let x = i as f64 * k;
            let j = x as usize;
            let f = (x - j as f64) as f32;
            *o = match (src.get(j), src.get(j + 1)) {
                (Some(a), Some(b)) => a + (b - a) * f,
                _ => 0.0,
            };
        }
    }

    for _ in 0..6 {
        if let Some(d) = write_once(root, &chart, &specs, &audio, label, cents) {
            return d;
        }
    }
    panic!("the bench tape stayed incomplete");
}

/// One try at writing the tape; `None` when the raw ring overflowed.
fn write_once(
    root: &Path,
    chart: &Chart,
    specs: &[StrumSpec],
    audio: &[f32],
    label: &str,
    cents: f64,
) -> Option<PathBuf> {
    let dir = root.join("tape");
    let _ = fs::remove_dir_all(&dir);
    let (rings, mut outs) = CaptureRings::new(f64::from(FS));
    let mut st = CaptureStage::new(f64::from(FS), 1, ChannelPick::Index(0), 512, rings);
    let raw = outs.take_raw().unwrap();
    outs.set_raw(true);
    let w = TapeWriter::create(&dir, TapeHeader::new(FS, FS), Some(raw)).unwrap();

    w.event(TapeEvent::Take {
        label: label.to_string(),
    });
    w.event(TapeEvent::Anchor {
        out_frame: 0,
        chart_json: chart.to_json(),
        timing: None,
        judge: None,
    });
    for (i, sp) in specs.iter().enumerate() {
        let t_in = sp.t0 * f64::from(FS) / 2f64.powf(cents / 1200.0);
        let note = |event, at_in| OnsetNote {
            event,
            t_in,
            at_in,
            judged: true,
            epoch: 0,
        };
        w.event(TapeEvent::Engine(EngineEvent::Onset(note(
            OnsetEvent::Heard {
                t: t_in,
                at: t_in + 256.0,
            },
            Some(t_in + 256.0),
        ))));
        w.event(TapeEvent::Engine(EngineEvent::Onset(note(
            OnsetEvent::Strum(Strum {
                t: t_in,
                spread: SPREAD_S * f64::from(FS),
                peaks: 6,
                strength: 3.0,
                rise_db: 18.0,
            }),
            None,
        ))));
        w.event(TapeEvent::Out(JudgeOut::Verdict(PulseVerdict {
            id: i as u32,
            attack: Attack::Heard,
            timing: Timing::Center,
            dt_ms: Some(0.0),
            dir: Direction::NotJudged(Why::GateLocked),
            onset_in: Some(t_in),
            pos_in_bar: 0,
            fin: true,
        })));
    }
    for (k, c) in audio.chunks(480).enumerate() {
        st.process(c, chunk_stamp((k * 480) as u64, c.len()));
        // The bench outruns the tape thread otherwise (raw ring overflow, tape incomplete).
        std::thread::sleep(Duration::from_micros(300));
    }
    st.flush();
    // The writer drains every 10 ms.
    std::thread::sleep(Duration::from_millis(120));
    let sum = w.finish().unwrap();
    sum.complete.then_some(dir)
}

fn pair<'a>(v: &'a Value, chord: &str) -> &'a Value {
    v["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["chord"] == chord)
        .unwrap_or_else(|| panic!("no pair for {chord}: {v}"))
}

#[test]
fn chord_tape_gives_one_row_per_chord_and_judges_nothing() {
    let tmp = Tmp::new("g8");
    let g = voicing("G");
    let tape = make_tape(&tmp.0, &[g; 8], "chords", None);
    let (code, v) = json(&["verify", tape.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["events_total"], 8, "{v}");
    assert_eq!(v["chord_gate_locked"], true);
    assert_eq!(v["judged"], false);
    assert_eq!(v["pairs"].as_array().unwrap().len(), 1, "{v}");
    let p = pair(&v, "G");
    assert_eq!(p["take"], "chords");
    assert_eq!(p["n"], 8);
    // The counts add up: every strum is exactly one of supports, names, abstain, detuned.
    let abstained: u64 = p["abstain"]
        .as_object()
        .unwrap()
        .values()
        .map(|x| x.as_u64().unwrap())
        .sum();
    assert_eq!(
        p["supports"].as_u64().unwrap()
            + p["names"].as_u64().unwrap()
            + abstained
            + p["detuned"].as_u64().unwrap(),
        8,
        "{p}"
    );
    // A chord played as written is recognised on most strums (dev-grid rate is 85%).
    assert!(p["supports"].as_u64().unwrap() >= 6, "{p}");
    assert_eq!(p["names"], 0, "{p}");
    // Locked chord gate: nothing is judged and no weight is given.
    for e in v["events"].as_array().unwrap() {
        assert_eq!(e["status"], "guia", "{e}");
        assert_eq!(e["judged"], false, "{e}");
        assert_eq!(e["weight"], 0.0, "{e}");
    }
    assert_eq!(p["ok"], 0);
    assert!(v["hints"].as_array().unwrap().is_empty());
}

#[test]
fn wrong_chord_is_not_recognised_and_never_judged_while_locked() {
    let tmp = Tmp::new("mix");
    let (g, c) = (voicing("G"), voicing("C"));
    let tape = make_tape(&tmp.0, &[g, g, g, g, c, c, c, c], "mix", None);
    let (code, v) = json(&["verify", tape.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{v}");
    let ev = v["events"].as_array().unwrap();
    assert_eq!(ev.len(), 8);
    let supports = |range: std::ops::Range<usize>| {
        range
            .filter(|&i| ev[i]["read"]["kind"] == "supports")
            .count()
    };
    // The four right strums are mostly recognised; the four played as C almost never.
    assert!(supports(0..4) >= 3, "{v}");
    assert!(supports(4..8) <= 1, "{v}");
    assert!(ev.iter().all(|e| e["status"] == "guia"), "{v}");
    assert!(ev.iter().all(|e| e["judged"] == false), "{v}");
}

#[test]
fn open_gate_gives_half_weight_ok_at_chord_level_and_never_an_accusation() {
    let tmp = Tmp::new("open");
    let (g, c) = (voicing("G"), voicing("C"));
    let tape = make_tape(&tmp.0, &[g, g, g, g, c, c, c, c], "mix", None);
    // A gate table with the chord gate open (written here, never by the command).
    let mut gates = guitarness_core::gates::GateTable::builtin();
    let mut e = gates
        .entry(guitarness_core::verdict::VerdictType::ChordIdentity)
        .clone();
    e.state = guitarness_core::gates::GateState::Open;
    // Evidence its own rule accepts, or `from_json` locks the claim again.
    e.evidence.events = e.rule.min_events + 100;
    e.evidence.real_tapes = e.rule.min_real_tapes + 3;
    e.evidence.false_wrong = 0;
    gates.set(guitarness_core::verdict::VerdictType::ChordIdentity, e);
    let gf = tmp.0.join("gates.json");
    fs::write(&gf, gates.to_json()).unwrap();
    let before = fs::read_to_string(&gf).unwrap();
    let (code, v) = json(&[
        "verify",
        tape.to_str().unwrap(),
        "--gates",
        gf.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["chord_gate_locked"], false);
    let ev = v["events"].as_array().unwrap();
    let oks: Vec<&Value> = ev.iter().filter(|e| e["status"] == "ok").collect();
    assert!(oks.len() >= 3, "{v}");
    for e in &oks {
        assert_eq!(e["weight"], 0.5, "{e}");
    }
    // One witness cannot accuse: no event is a mismatch.
    assert!(ev.iter().all(|e| e["status"] != "nao_confere"), "{v}");
    // The gate file is read, never written.
    assert_eq!(fs::read_to_string(&gf).unwrap(), before);
}

#[test]
fn default_text_has_no_digits_and_details_add_the_table() {
    let tmp = Tmp::new("text");
    let g = voicing("G");
    let tape = make_tape(&tmp.0, &[g; 6], "chords", None);
    let (code, out, _) = guitarness(&["verify", tape.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(!out.trim().is_empty());
    assert!(
        !out.chars().any(|c| c.is_ascii_digit()),
        "digits in default text: {out}"
    );
    assert!(out.contains("guia"), "{out}");
    for bad in ["errado", "falhou", "erro", "perfeito"] {
        assert!(!out.contains(bad), "{out}");
    }
    let (code, det, _) = guitarness(&["verify", tape.to_str().unwrap(), "--detalhes"]);
    assert_eq!(code, 0);
    assert!(det.starts_with(&out), "{det}");
    assert!(det.contains("apoia") && det.contains("| G |"), "{det}");
}

#[test]
fn tape_without_strums_gives_empty_output_and_code_zero() {
    let tmp = Tmp::new("empty");
    // A chart with no strum at all: no event, so no row.
    let g = voicing("G");
    let tape = make_tape(&tmp.0, &[g; 2], "none", Some(0));
    // Chart has no events; the verdicts name ids it does not hold, so nothing is read.
    let (code, out, err) = guitarness(&["verify", tape.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(out.is_empty(), "{out:?}");
    let (code, v) = json(&["verify", tape.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["events_total"], 0);
    assert!(v["pairs"].as_array().unwrap().is_empty());
}

#[test]
fn bad_arguments_and_unreadable_tapes_have_their_own_codes() {
    let tmp = Tmp::new("args");
    let (code, _, err) = guitarness(&["verify"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, _) = guitarness(&["verify", "x", "--nope"]);
    assert_eq!(code, 2);
    let (code, _, _) = guitarness(&["verify", "x", "--gates"]);
    assert_eq!(code, 2);
    let (code, v) = json(&["verify", tmp.0.to_str().unwrap(), "--json"]);
    assert_eq!(code, 20, "{v}");
    assert_eq!(v["ok"], false);
    let (code, out, _) = guitarness(&["verify", "--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("EXIT CODES"));
}

#[test]
fn detuned_strums_are_neither_right_nor_wrong_and_suggest_the_tuner() {
    let tmp = Tmp::new("detuned");
    let g = voicing("G");
    // 38 cents sharp: at the edge of what the witness's tuning search covers (it reaches
    // 40 cents either side), so the strums are reported as detuned.
    let tape = make_tape_with(&tmp.0, &[g; 6], "chords", None, 38.0);
    let (code, v) = json(&["verify", tape.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{v}");
    let ev = v["events"].as_array().unwrap();
    let detuned = ev.iter().filter(|e| e["status"] == "desafinado").count();
    assert!(detuned >= 3, "{v}");
    assert_eq!(v["suggest_tune"], true, "{v}");
    for e in ev.iter().filter(|e| e["status"] == "desafinado") {
        assert_eq!(e["judged"], false);
        assert_eq!(e["weight"], 0.0);
    }
    let (_, out, _) = guitarness(&["verify", tape.to_str().unwrap()]);
    assert!(out.contains("guitarness tune"), "{out}");
    assert!(!out.contains("não confere"), "{out}");
}

#[test]
fn a_repeated_wrong_fret_gives_a_hint_that_is_not_a_verdict() {
    let tmp = Tmp::new("hint");
    // G with the low string fretted one fret high, four times in a row.
    let off = SimVoicing([4, 2, 0, 0, 0, 3]);
    let tape = make_tape(&tmp.0, &[off; 4], "off", None);
    let (code, v) = json(&["verify", tape.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{v}");
    let hints = v["hints"].as_array().unwrap();
    assert_eq!(hints.len(), 1, "{v}");
    assert_eq!(hints[0]["kind"], "WrongFret s0 f4", "{v}");
    assert_eq!(hints[0]["string"], 0);
    for h in hints {
        assert_eq!(h["verdict"], false);
        assert!(h["run"].as_u64().unwrap() >= 2);
        assert!(!h["kind"].as_str().unwrap().starts_with("Neighbour"));
    }
    assert!(
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["judged"] == false)
    );
    // The player text speaks of the chord only: no string, no fret, no accusation.
    let (_, out, _) = guitarness(&["verify", tape.to_str().unwrap()]);
    assert!(out.contains("guia"), "{out}");
    for word in ["corda", "casa", "WrongFret", "s0"] {
        assert!(!out.contains(word), "{out}");
    }
}
