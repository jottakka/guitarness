//! `guitarness tune --fake` runs the real command on synthetic strings from `guitarness-sim` through the
//! DSP `Pipeline` (plan M2 T13, Pulso L4). No test opens a real audio device or the microphone.

use std::process::{Command, Output};

use guitarness_dsp::{AcfMode, Pipeline, mtof};
use guitarness_sim::string::{Pluck, render};
use guitarness_sim::strum::OPEN;
use guitarness_sim::synth::Mulberry32;

fn exec(args: &[&str], forbid: bool) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_guitarness"));
    c.arg("tune").args(args);
    if forbid {
        c.env("GUITARNESS_FORBID_DEVICES", "1");
    } else {
        c.env_remove("GUITARNESS_FORBID_DEVICES");
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
fn t13_fake_cli_measures_all_strings_within_3_cents() {
    let (code, v) = json(&["--fake", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["fake"], true, "{v}");
    let notes = v["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 18, "6 strings x 3 offsets");

    let mut max_err = 0.0f64;
    for item in notes {
        let exp_midi = item["expected_midi"].as_i64().unwrap();
        let got_midi = item["midi"].as_i64().expect("midi detected");
        assert_eq!(got_midi, exp_midi, "{item}");
        let err = item["error_cents"].as_f64().expect("error_cents");
        let exp_c = item["expected_cents"].as_f64().unwrap();
        let got_c = item["cents"].as_f64().unwrap();
        println!(
            "string {} ({}) expected {:+5.1} cents -> got {:+6.2} cents (err {:.3} cents)",
            item["string"], item["note"], exp_c, got_c, err
        );
        assert!(err <= 3.0, "error {err} > 3 cents in {item}");
        max_err = max_err.max(err);
    }
    let reported_max = v["max_error_cents"].as_f64().unwrap();
    assert!((reported_max - max_err).abs() < 1e-9);
    assert!(max_err <= 3.0, "max error {max_err}");
}

#[test]
fn t13_direct_pipeline_matches_guitarness_sim_strings() {
    let sr = 48_000.0;
    let mut rng = Mulberry32::new(42);
    let len = (0.65 * sr) as usize;
    let mut max_err = 0.0f64;
    for &midi in &OPEN {
        for &offset_cents in &[-20.0f64, 0.0, 15.0] {
            let freq = mtof(f64::from(midi) + offset_cents / 100.0, 440.0);
            let mut p = Pluck::new(freq);
            p.gain = 0.25;
            let audio = render(sr, &p, len, None, &[], &mut rng);

            let mut pipe = Pipeline::demo(sr, AcfMode::Fft, 256);
            let mut cents_samples = Vec::new();
            for (b, block) in audio.chunks(256).enumerate() {
                let t0 = (b * 256) as f64 / sr;
                let mut steady = false;
                pipe.process(
                    block,
                    t0,
                    |fr| {
                        // Steady region after initial window fill and pluck burst.
                        if fr.t >= 0.12 && fr.c >= 0.86 {
                            steady = true;
                        }
                    },
                    |_| {},
                );
                if steady && pipe.tracker().current() == Some(midi) {
                    cents_samples.push(pipe.tracker().cents());
                }
            }
            assert!(!cents_samples.is_empty(), "no steady frames for {midi}");
            cents_samples.sort_by(f64::total_cmp);
            let med = cents_samples[cents_samples.len() / 2];
            let err = (med - offset_cents).abs();
            assert!(
                err <= 3.0,
                "midi {midi} offset {offset_cents}: measured {med:.2} (err {err:.2})"
            );
            max_err = max_err.max(err);
        }
    }
    assert!(max_err <= 3.0);
}

#[test]
fn fake_single_string_and_cents_and_a4() {
    for (s, c) in [(6, -20.0), (3, 0.0), (1, 15.0)] {
        let s_str = s.to_string();
        let c_str = c.to_string();
        let (code, v) = json(&["--fake", "--json", "--string", &s_str, "--cents", &c_str]);
        assert_eq!(code, 0, "{v}");
        let notes = v["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 1);
        let err = notes[0]["error_cents"].as_f64().unwrap();
        assert!(err <= 3.0, "string {s} cents {c}: {v}");
    }

    let (code, v) = json(&[
        "--fake", "--json", "--string", "5", "--cents", "0", "--a4", "432",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["a4_hz"], 432.0);
    let err = v["notes"][0]["error_cents"].as_f64().unwrap();
    assert!(err <= 3.0, "{v}");
}

#[test]
fn pt_br_text_has_note_and_cents_without_red_or_failure_words() {
    let out = exec(&["--fake"], true);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(!text.contains('\x1b'), "no ANSI colors/red: {text}");
    for expected in [
        "E2", "A2", "D3", "G3", "B3", "E4", "cents", "afinada", "grave", "aguda",
    ] {
        assert!(text.contains(expected), "missing '{expected}' in:\n{text}");
    }
    let lower = text.to_lowercase();
    for bad in [
        "falh", "erro", "fracasso", "errou", "errad", "ruim", "vermelho", "red",
    ] {
        assert!(!lower.contains(bad), "forbidden word '{bad}' in:\n{text}");
    }

    let out_det = exec(
        &["--fake", "--detalhes", "--string", "6", "--cents", "0"],
        true,
    );
    assert_eq!(out_det.status.code(), Some(0));
    let det = String::from_utf8_lossy(&out_det.stdout).to_string();
    assert!(det.contains("Hz") && det.contains("clareza"), "{det}");
    let det_lower = det.to_lowercase();
    for bad in ["falh", "erro", "fracasso", "errou"] {
        assert!(
            !det_lower.contains(bad),
            "forbidden word '{bad}' in:\n{det}"
        );
    }
}

#[test]
fn forbidden_devices_exit_12_before_opening_anything() {
    for args in [
        &[][..],
        &["--beep"][..],
        &["--json"][..],
        &["--json", "--beep"][..],
        &["--in", "fake-in"][..],
    ] {
        let out = exec(args, true);
        assert_eq!(out.status.code(), Some(12), "{args:?}");
        assert!(!String::from_utf8_lossy(&out.stdout).contains('\x1b'));
    }
    let out = exec(&["--json"], true);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["exit"], 12);
}

#[test]
fn help_and_bad_command_line() {
    let help = exec(&["--help"], true);
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("guitarness tune"));

    for bad in [
        &["--nope"][..],
        &["--speed", "2"][..],
        &["--cents", "10"][..],
        &["--string", "3"][..],
        &["--fake", "--string", "0"][..],
        &["--fake", "--string", "7"][..],
        &["--fake", "--cents", "80"][..],
        &["--a4", "0"][..],
        &["--seconds", "0"][..],
    ] {
        assert_eq!(exec(bad, true).status.code(), Some(2), "{bad:?}");
    }
}
