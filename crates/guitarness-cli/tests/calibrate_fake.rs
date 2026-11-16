//! `guitarness calibrate --fake` runs the real command code on the fake backend. No test opens
//! a real device: either `--fake`, or `GUITARNESS_FORBID_DEVICES` stops the command first.
//! Profiles are written into a temp dir through `--profile`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "guitarness-calibrate-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn exec(args: &[&str], forbid: bool) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_guitarness"));
    c.arg("calibrate").args(args);
    if forbid {
        c.env("GUITARNESS_FORBID_DEVICES", "1");
    }
    c.output().expect("run guitarness")
}

fn json(args: &[&str]) -> (i32, serde_json::Value) {
    let out = exec(args, true);
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
fn fake_json_finds_the_loop_latency() {
    let dir = tmp("json");
    let prof = dir.join("profiles.json");
    let (code, v) = json(&["--fake", "--json", "--profile", prof.to_str().unwrap()]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["basis"], "loopback");
    assert_eq!(v["judgeable"], true);
    let l = v["l_rt_ms"].as_f64().unwrap();
    assert!((l - 35.0).abs() <= 0.3, "L_rt {l}");
    // The profile was saved (v2) and reads back.
    assert_eq!(v["saved"].as_str().unwrap(), prof.to_str().unwrap());
    let text = std::fs::read_to_string(&prof).unwrap();
    assert!(
        text.contains("\"version\": 2") || text.contains("\"version\":2"),
        "{text}"
    );
    // A second run on the same profile is the quick check and keeps it.
    let (code, v2) = json(&["--fake", "--json", "--profile", prof.to_str().unwrap()]);
    assert_eq!(code, 0, "{v2}");
    assert_eq!(v2["runs"][0]["outcome"], "kept", "{v2}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn fake_repeat_has_a_tight_range() {
    let (code, v) = json(&["--fake", "--json", "--repeat", "3"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["runs"].as_array().unwrap().len(), 3);
    let range = v["range_ms"].as_f64().unwrap();
    assert!(range <= 0.1, "range {range} ms");
    assert!((v["l_rt_ms"].as_f64().unwrap() - 35.0).abs() <= 0.3);
    assert!(v["saved"].is_null(), "fake without --profile must not save");
}

#[test]
fn fake_short_is_short_and_exact() {
    let (code, full) = json(&["--fake", "--json"]);
    assert_eq!(code, 0, "{full}");
    let (code, v) = json(&["--fake", "--json", "--short"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["basis"], "loopback");
    let l = v["l_rt_ms"].as_f64().unwrap();
    assert!((l - 35.0).abs() <= 0.3, "L_rt {l}");
    let (s, f) = (
        v["runs"][0]["seconds"].as_f64().unwrap(),
        full["runs"][0]["seconds"].as_f64().unwrap(),
    );
    assert!(s <= 6.0 && s < f, "short {s} s, full {f} s");
}

#[test]
fn forbidden_devices_exit_12_before_opening_anything() {
    for args in [&[][..], &["--beep"][..], &["--json", "--beep"][..]] {
        let out = exec(args, true);
        assert_eq!(out.status.code(), Some(12), "{args:?}");
    }
    let out = exec(&["--json"], true);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["exit"], 12);
}

#[test]
fn real_devices_need_beep() {
    // Without the forbid variable this would open a device if --beep were given, so only
    // the refusal path is run, and only where nothing is forbidden by the environment.
    if std::env::var_os("GUITARNESS_FORBID_DEVICES").is_some() {
        return;
    }
    let out = exec(&[], false);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn default_text_has_no_numbers_and_details_have_them() {
    let out = exec(&["--fake"], true);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("Pronto"), "{text}");
    assert!(!text.chars().any(|c| c.is_ascii_digit()), "{text}");
    let lower = text.to_lowercase();
    for bad in ["falh", "erro", "fracasso", "errou"] {
        assert!(!lower.contains(bad), "{text}");
    }
    let out = exec(&["--fake", "--detalhes"], true);
    let det = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(det.contains("L_rt") && det.contains("35."), "{det}");
}

#[test]
fn bad_command_line_exits_2() {
    assert_eq!(exec(&["--nope"], true).status.code(), Some(2));
    assert_eq!(exec(&["--speed", "2"], true).status.code(), Some(2));
    assert_eq!(
        exec(&["--fake", "--repeat", "0"], true).status.code(),
        Some(2)
    );
}
