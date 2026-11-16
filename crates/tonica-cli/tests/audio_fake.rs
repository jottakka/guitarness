//! `probe --fake` and `soak --fake` run the real command code on the fake backend.
//! `audio devices` is not run here: it enumerates real hardware.

use std::process::Command;

fn run(args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_tonica"))
        .args(args)
        .output()
        .expect("run tonica");
    let text = String::from_utf8_lossy(&out.stdout);
    let v = serde_json::from_str(text.trim()).unwrap_or_else(|e| {
        panic!(
            "bad json ({e}): {text}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().unwrap_or(-1), v)
}

#[test]
fn probe_fake_json() {
    let (code, v) = run(&[
        "audio",
        "probe",
        "--fake",
        "--json",
        "--seconds",
        "2",
        "--speed",
        "4",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["mode"], "probe");
    assert_eq!(v["fake"], true);
    assert_eq!(v["beep"], false);
    assert_eq!(v["rates"]["in_hz"], 48000);
    assert!(v["callbacks"]["in"].as_u64().unwrap() > 50);
    assert!(v["callbacks"]["out"].as_u64().unwrap() > 50);
    assert_eq!(v["xruns"]["in"], 0);
    assert_eq!(v["xruns"]["out"], 0);
    assert_eq!(v["drops"]["total"], 0);
    assert_ne!(v["clock"]["quality"], "broken");
    assert!(v["clock"]["origin_offset_ns"].is_i64());
}

#[test]
fn soak_fake_json_with_probes() {
    let (code, v) = run(&[
        "audio",
        "soak",
        "--fake",
        "--json",
        "--minutes",
        "0.1",
        "--beep",
        "--probe-every",
        "1",
        "--speed",
        "4",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["mode"], "soak");
    assert_eq!(v["checks"]["pass"], true);
    assert_eq!(v["mixer"]["late"], 0);
    assert_eq!(v["stamps"]["steps_in"], 0);
    let l = &v["loop"];
    assert!(l["sent"].as_u64().unwrap() >= 2, "{l}");
    assert!(l["heard_pct"].as_f64().unwrap() >= 95.0, "{l}");
    let mean = l["mean_ms"].as_f64().unwrap();
    assert!((mean - 35.0).abs() < 5.0, "L_rt {mean} ms");
}

#[test]
fn soak_without_minutes_is_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_tonica"))
        .args(["audio", "soak", "--fake"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn probe_every_needs_beep() {
    let out = Command::new(env!("CARGO_BIN_EXE_tonica"))
        .args([
            "audio",
            "soak",
            "--fake",
            "--minutes",
            "0.1",
            "--probe-every",
            "1",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

// ------------------------------------------------------------------ record

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("tonica-l3-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn record_fake_writes_a_complete_tape_with_the_take() {
    let dir = scratch("take");
    let (code, v) = run(&[
        "audio",
        "record",
        dir.to_str().unwrap(),
        "--fake",
        "--json",
        "--seconds",
        "2",
        "--take",
        "primeira",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["mode"], "record");
    assert_eq!(v["complete"], true);
    assert_eq!(v["audio"], true);
    assert_eq!(v["beep"], false);
    assert_eq!(v["take"], "primeira");
    let tape = std::path::PathBuf::from(v["tape"].as_str().unwrap());
    assert!(tape.starts_with(&dir), "{tape:?}");

    let header = std::fs::read_to_string(tape.join("tape.json")).unwrap();
    assert!(header.contains("\"label\":\"primeira\""), "{header}");
    assert!(header.contains("\"complete\": true") || header.contains("\"complete\":true"));

    // 2 s of microphone, within one block (the fake delivers 256 to 1024 frames).
    let r = hound::WavReader::open(tape.join("mic.wav")).unwrap();
    let spec = r.spec();
    let secs = f64::from(r.duration()) / f64::from(spec.sample_rate);
    assert!(
        (secs - 2.0).abs() <= 1024.0 / f64::from(spec.sample_rate),
        "{secs} s"
    );
    assert_eq!(v["mic_frames"].as_u64().unwrap(), u64::from(r.duration()));

    // The tape opens and reads back as complete.
    let t = tonica_session::reader::Tape::open(&tape).expect("tape opens");
    assert!(t.complete);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn record_fake_with_click_and_beep_stays_complete() {
    let dir = scratch("click");
    let (code, v) = run(&[
        "audio",
        "record",
        dir.to_str().unwrap(),
        "--fake",
        "--json",
        "--seconds",
        "1",
        "--click",
        "120",
        "--beep",
        "--speed",
        "4",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["beep"], true);
    assert_eq!(v["complete"], true);
    assert_eq!(v["take"], "take");
    assert_eq!(v["refused"].as_array().unwrap().len(), 0, "{v}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn record_usage_errors_exit_2() {
    let exit = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tonica"))
            .args(args)
            .output()
            .unwrap()
            .status
            .code()
    };
    assert_eq!(exit(&["audio", "record", "--fake"]), Some(2), "no folder");
    assert_eq!(
        exit(&[
            "audio", "record", "x", "--fake", "--click", "90", "--chart", "c.json"
        ]),
        Some(2),
        "chart and click"
    );
    assert_eq!(
        exit(&[
            "audio",
            "record",
            "x",
            "--fake",
            "--chart",
            "/nonexistent/c.json"
        ]),
        Some(2),
        "missing chart"
    );
    assert_eq!(
        exit(&["audio", "probe", "--fake", "--take", "x"]),
        Some(2),
        "record-only option on probe"
    );
}

#[test]
fn record_refuses_real_devices_when_forbidden() {
    let dir = scratch("forbid");
    let out = Command::new(env!("CARGO_BIN_EXE_tonica"))
        .args(["audio", "record", dir.to_str().unwrap(), "--json"])
        .env("TONICA_FORBID_DEVICES", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(12));
    assert!(!dir.exists(), "nothing was opened or written");
}

#[test]
fn record_without_chart_judges_nothing_and_keeps_the_take() {
    let dir = scratch("nochart");
    let (code, v) = run(&[
        "audio",
        "record",
        dir.to_str().unwrap(),
        "--fake",
        "--json",
        "--seconds",
        "1",
        "--click",
        "120",
        "--take",
        "livre",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["judged_events"], 0, "{v}");
    assert_eq!(v["complete"], true, "{v}");
    let tape = std::path::PathBuf::from(v["tape"].as_str().unwrap());
    let events = std::fs::read_to_string(tape.join("events.jsonl")).unwrap();
    assert!(events.contains("\"take\""), "no Take line:\n{events}");
    assert!(events.contains("livre"), "{events}");
    assert!(
        !events.contains("\"judge\""),
        "judged line on tape:\n{events}"
    );
    assert!(
        !events.contains("\"anchor\""),
        "round anchor on tape:\n{events}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
