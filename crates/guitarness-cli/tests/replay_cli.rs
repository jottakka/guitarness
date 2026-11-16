//! `guitarness replay` on tapes made by `guitarness audio record --fake` (no device, no microphone).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use guitarness_core::chart::{Chart, Dir, Event, EventKind, Style, Voicing};
use serde_json::Value;

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "guitarness-l1-{}-{}-{name}",
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

/// `beats` down-strums, one per quarter note at `bpm`.
fn strum_chart(bpm: f64, beats: u32) -> Chart {
    let mut c = Chart::new("record", bpm);
    c.voicings.insert(
        "G".to_string(),
        Voicing {
            frets: [3, 2, 0, 0, 0, 3],
            req: None,
            fingers: [0; 6],
            root: Some(7),
        },
    );
    for i in 0..beats {
        c.tiers[0].events.push(Event {
            tick: i * c.tpq,
            dur: c.tpq,
            kind: EventKind::Strum {
                dir: Dir::Down,
                span: None,
                style: Style::Full,
                roll_ms: None,
            },
            chord: Some("G".to_string()),
        });
    }
    c
}

/// Records a 2 s tape on the fake backend inside `root`; returns the tape folder.
fn make_tape(root: &Path, take: &str) -> PathBuf {
    // `record` without a chart judges nothing, so the tape needs a real chart.
    let chart_file = root.join(format!("{take}-chart.json"));
    fs::write(&chart_file, strum_chart(60.0, 8).to_json()).unwrap();
    let (code, v) = json(&[
        "audio",
        "record",
        root.to_str().unwrap(),
        "--chart",
        chart_file.to_str().unwrap(),
        "--fake",
        "--seconds",
        "2",
        "--speed",
        "8",
        "--take",
        take,
        "--json",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["complete"], true, "{v}");
    PathBuf::from(v["tape"].as_str().unwrap())
}

fn events_path(tape: &Path) -> PathBuf {
    tape.join("events.jsonl")
}

fn append_line(tape: &Path, line: &str) {
    let p = events_path(tape);
    let mut s = fs::read_to_string(&p).unwrap();
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(line);
    s.push('\n');
    fs::write(p, s).unwrap();
    // Keep the header's count right, or the reader rightly calls the tape damaged.
    let hp = tape.join("tape.json");
    let mut h: Value = serde_json::from_str(&fs::read_to_string(&hp).unwrap()).unwrap();
    let n = h["counts"]["events"].as_u64().unwrap();
    h["counts"]["events"] = (n + 1).into();
    fs::write(hp, serde_json::to_string_pretty(&h).unwrap()).unwrap();
}

/// Next free `seq` of the tape.
fn next_seq(tape: &Path) -> u64 {
    fs::read_to_string(events_path(tape))
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["seq"].as_u64())
        .max()
        .unwrap()
        + 1
}

/// Changes the recorded verdict of event 0 so that today's judge disagrees.
fn tamper_verdict(tape: &Path, to_attack: &str) {
    let p = events_path(tape);
    let s = fs::read_to_string(&p).unwrap();
    let old = "\"attack\":\"nj:no_calibration\"";
    assert!(s.contains(old), "the tape has the verdict this test edits");
    fs::write(
        &p,
        s.replacen(old, &format!("\"attack\":\"{to_attack}\""), 1),
    )
    .unwrap();
}

#[test]
fn same_code_means_no_difference_exit_0() {
    let tmp = Tmp::new("exact");
    let tape = make_tape(&tmp.0, "take");
    let (code, v) = json(&["replay", tape.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["exit"], 0);
    assert_eq!(v["level"], "events");
    assert_eq!(v["tapes"][0]["exact"], true);
    assert_eq!(v["tapes"][0]["diffs"].as_array().unwrap().len(), 0);
    assert_eq!(v["tapes"][0]["complete"], true);
    // A folder of tapes works as well, and the audio level agrees.
    let (code, v) = json(&["replay", tmp.0.to_str().unwrap(), "--audio", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["level"], "audio");
    assert_eq!(v["tapes"][0]["onsets"]["exact"], true);
}

#[test]
fn default_text_is_portuguese_without_numbers() {
    let tmp = Tmp::new("text");
    let tape = make_tape(&tmp.0, "take");
    let (code, out, _) = guitarness(&["replay", tape.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.contains("Tudo igual"), "{out}");
    assert!(!out.chars().any(|c| c.is_ascii_digit()), "{out}");
}

#[test]
fn a_changed_verdict_is_a_difference_exit_30() {
    let tmp = Tmp::new("diff");
    let tape = make_tape(&tmp.0, "take");
    tamper_verdict(&tape, "heard");
    let (code, v) = json(&["replay", tape.to_str().unwrap(), "--json"]);
    assert_eq!(code, 30, "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["exit"], 30);
    assert_eq!(v["tapes"][0]["exact"], false);
    let diffs = v["tapes"][0]["diffs"].as_array().unwrap();
    assert!(!diffs.is_empty(), "{v}");
    assert!(diffs[0]["recorded"].as_str().unwrap().contains("Heard"));
    // The text says so and --detalhes lists the difference.
    let (code, out, _) = guitarness(&["replay", tape.to_str().unwrap(), "--detalhes"]);
    assert_eq!(code, 30);
    assert!(out.contains("diferença"), "{out}");
    assert!(out.contains("gravado"), "{out}");
}

#[test]
fn a_failed_gate_is_exit_30() {
    let tmp = Tmp::new("gate");
    let tape = make_tape(&tmp.0, "take");
    // Label the one event as played on time, then record that it was not heard.
    let seq = next_seq(&tape);
    append_line(
        &tape,
        &format!(
            "{{\"seq\":{seq},\"k\":\"truth\",\"id\":0,\"kind\":\"strum\",\"out_frame\":30000,\"played\":true,\
\"extra\":false,\"muted\":false,\"g_in\":0.0,\"arrival_in\":0.0,\"onset_in\":0.0,\"dt_ms\":0.0}}"
        ),
    );
    tamper_verdict(&tape, "not_heard");
    // A gate table that asks for one event and no mistakes.
    let gates = tmp.0.join("gates.json");
    let (_, _, _) = guitarness(&[
        "replay",
        "--gates",
        gates.to_str().unwrap(),
        make_tape(&tmp.0, "take").to_str().unwrap(),
    ]);
    let mut table: Value = serde_json::from_str(&fs::read_to_string(&gates).unwrap()).unwrap();
    table["gates"]["attack"]["rule"]["min_events"] = 1.into();
    table["gates"]["attack"]["rule"]["max_false_wrong_per"] = 1000.into();
    fs::write(&gates, table.to_string()).unwrap();
    // The folder holds two tapes; replay only the labeled one.
    let (code, v) = json(&[
        "replay",
        tape.to_str().unwrap(),
        "--report",
        "--gates",
        gates.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 30, "{v}");
    assert_eq!(v["gate_failed"], true, "{v}");
    let attack = v["report"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["type"] == "attack")
        .unwrap();
    assert_eq!(attack["gate"], "fail", "{attack}");
    assert!(v["gates_written"].is_string());
}

#[test]
fn report_counts_the_tape_and_leaves_unlabeled_rounds_out() {
    let tmp = Tmp::new("report");
    let tape = make_tape(&tmp.0, "take");
    let (code, v) = json(&["replay", tape.to_str().unwrap(), "--report", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["gate_failed"], false);
    assert_eq!(v["report"]["types"].as_array().unwrap().len(), 10);
    let (code, out, _) = guitarness(&["replay", tape.to_str().unwrap(), "--report"]);
    assert_eq!(code, 0);
    assert!(out.contains("unlabeled rounds left out: 1"), "{out}");
}

#[test]
fn bad_command_lines_exit_2_and_unreadable_tapes_exit_20() {
    let tmp = Tmp::new("bad");
    assert_eq!(guitarness(&["replay"]).0, 2);
    assert_eq!(guitarness(&["replay", "--nope", "x"]).0, 2);
    assert_eq!(
        guitarness(&["replay", "x", "--export-dispute", "0", "a.wav"]).0,
        2
    );
    // A folder with no tape in it.
    let (code, v) = json(&["replay", tmp.0.to_str().unwrap(), "--json"]);
    assert_eq!(code, 2, "{v}");
    // A tape that is not one.
    let broken = tmp.0.join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("tape.json"), "not json").unwrap();
    let (code, v) = json(&["replay", broken.to_str().unwrap(), "--json"]);
    assert_eq!(code, 20, "{v}");
    assert_eq!(v["ok"], false);
    // No microphone on the tape: --audio cannot run.
    let tape = make_tape(&tmp.0, "take");
    fs::remove_file(tape.join("mic.wav")).unwrap();
    let (code, _, _) = guitarness(&["replay", tape.to_str().unwrap(), "--audio"]);
    assert_eq!(code, 20);
}

#[test]
fn export_dispute_writes_one_second_each_side() {
    let tmp = Tmp::new("dispute");
    let tape = make_tape(&tmp.0, "take");
    let seq = next_seq(&tape);
    append_line(
        &tape,
        &format!("{{\"seq\":{seq},\"k\":\"cmd\",\"c\":\"dispute\"}}"),
    );
    let (_, v) = json(&["replay", tape.to_str().unwrap(), "--disputes", "--json"]);
    let list = v["disputes"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{v}");
    assert_eq!(list[0]["n"], 1);

    let wav = tmp.0.join("dispute.wav");
    let (_, v) = json(&[
        "replay",
        tape.to_str().unwrap(),
        "--export-dispute",
        "1",
        wav.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(v["exported"]["frames"], 96_000, "{v}");
    let r = hound::WavReader::open(&wav).unwrap();
    assert_eq!(r.spec().sample_rate, 48_000);
    assert_eq!(r.len(), 96_000);

    // A dispute that does not exist is a command-line error.
    let (code, _, _) = guitarness(&[
        "replay",
        tape.to_str().unwrap(),
        "--export-dispute",
        "2",
        wav.to_str().unwrap(),
    ]);
    assert_eq!(code, 2);
}

#[test]
fn render_out_writes_the_click_schedule() {
    let tmp = Tmp::new("render");
    let tape = make_tape(&tmp.0, "take");
    let wav = tmp.0.join("out.wav");
    let (code, v) = json(&[
        "replay",
        tape.to_str().unwrap(),
        "--render-out",
        wav.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{v}");
    assert!(v["rendered"]["beats"].as_u64().unwrap() >= 1, "{v}");
    let r = hound::WavReader::open(&wav).unwrap();
    assert_eq!(r.spec().sample_rate, 48_000);
    assert!(r.len() > 48_000);
}
