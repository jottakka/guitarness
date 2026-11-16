//! `tonica tune`: command-line guitar tuner on top of the DSP [`Pipeline`] (plan M2 T13, Pulso L4).
//!
//! Uses [`AudioEngine`] over [`CpalBackend`] for live input or, with `--fake`, feeds synthetic
//! plucked strings from `tonica-sim` through the engine's `Pipeline` on [`FakeBackend`]
//! (virtual time, no hardware). Nothing is ever played to the output. Output is short Brazilian
//! Portuguese text (never red, no failure wording) or `--json` for machine reading.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tonica_audio::{
    AudioEngine, AudioError, CpalBackend, EngineConfig, EngineEvent, EngineIo, FakeBackend,
    FakeConfig, OpenRequest, Pump, SoundBank,
};
use tonica_dsp::frontend::FrontendConfig;
use tonica_dsp::{Event, Tracker, TrackerParams, mtof};
use tonica_sim::string::{Pluck, render};
use tonica_sim::strum::OPEN;
use tonica_sim::synth::Mulberry32;

const HELP: &str = "\
tonica tune: command-line guitar tuner

USAGE
  tonica tune [--fake] [--json] [--detalhes] [--in ID] [--out ID]
              [--seconds N] [--a4 HZ] [--cents C] [--string N] [--speed X] [--beep]

OPTIONS
  --fake            run on synthetic guitar strings from tonica-sim through Pipeline (no hardware)
  --json            machine-readable report on stdout (no player text)
  --detalhes        also print measured frequencies and clarity
  --in ID / --out ID  device ids from `tonica audio devices` (default: system defaults)
  --seconds N       listening time in live mode, seconds (default: 5)
  --a4 HZ           concert pitch reference A4 in Hz (default: 440)
  --cents C         with --fake: single detune offset in cents (default: -20, 0 and +15)
  --string N        with --fake: guitar string 1..=6 (1=E4 .. 6=E2; default: all 6)
  --speed X         with --fake: virtual seconds per wall second (default: as fast as possible)
  --beep            accepted for parity with other commands (tune never emits sound)

Set TONICA_FORBID_DEVICES to refuse real devices (exit 12 before anything is opened).

EXIT CODES
   0  done
  10  no input device        11  no output device
  12  permission denied, stream failed, devices forbidden or interrupted
  13  unsupported sample rate or format
  20  other error
   2  bad command line
";

/// Default fake room noise RMS (-60 dBFS, same as `calibrate_cmd`).
const FAKE_NOISE: f32 = 1.0e-3;
/// Default fake pluck gain (comfortably above noise gate and below clipping).
const FAKE_GAIN: f32 = 0.25;
/// Ring time of each synthetic pluck in fake mode, seconds.
const RING_S: f64 = 0.50;
/// Total slot time per synthetic pluck (ring + silence so Tracker releases cleanly), seconds.
const SLOT_S: f64 = 0.72;
/// Virtual time step per manual pump slice, seconds.
const SLICE_S: f64 = 0.005;
/// Default T13 detune offsets in cents.
const T13_CENTS: [f64; 3] = [-20.0, 0.0, 15.0];
/// Minimum NSDF clarity for a pitch reading (matches `Tracker`'s `C_ON`).
const MIN_CLARITY: f64 = 0.86;
/// Tuning threshold within which a string is considered in tune, cents.
const IN_TUNE_CENTS: f64 = 3.0;

pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return 0;
    }
    let o = match Opts::parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}\n\n{HELP}");
            return 2;
        }
    };
    if !o.fake && std::env::var_os("TONICA_FORBID_DEVICES").is_some() {
        return report_error(
            &o,
            12,
            "real audio devices are forbidden (TONICA_FORBID_DEVICES is set)",
            "Os dispositivos de som reais estão desligados aqui. Nada foi aberto.",
        );
    }
    if o.fake { run_fake(&o) } else { run_live(&o) }
}

struct Opts {
    json: bool,
    fake: bool,
    details: bool,
    in_id: Option<String>,
    out_id: Option<String>,
    seconds: f64,
    a4: f64,
    cents: Option<f64>,
    string: Option<u8>,
    speed: Option<f64>,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut o = Opts {
            json: false,
            fake: false,
            details: false,
            in_id: None,
            out_id: None,
            seconds: 5.0,
            a4: 440.0,
            cents: None,
            string: None,
            speed: None,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = |name: &str| -> Result<String, String> {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            let pos_num = |name: &str, s: String| -> Result<f64, String> {
                s.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .ok_or_else(|| format!("{name}: '{s}' is not a positive number"))
            };
            match a.as_str() {
                "--json" => o.json = true,
                "--fake" => o.fake = true,
                "--detalhes" => o.details = true,
                "--beep" => {}
                "--in" => o.in_id = Some(val("--in")?),
                "--out" => o.out_id = Some(val("--out")?),
                "--seconds" => o.seconds = pos_num("--seconds", val("--seconds")?)?,
                "--a4" => {
                    let s = val("--a4")?;
                    let hz = pos_num("--a4", s)?;
                    if !(200.0..=880.0).contains(&hz) {
                        return Err(format!("--a4: '{hz}' must be in 200..=880 Hz"));
                    }
                    o.a4 = hz;
                }
                "--cents" => {
                    let s = val("--cents")?;
                    let c = s
                        .parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite() && (-50.0..=50.0).contains(v))
                        .ok_or_else(|| format!("--cents: '{s}' must be in -50..=50"))?;
                    o.cents = Some(c);
                }
                "--string" => {
                    let s = val("--string")?;
                    let n = s
                        .parse::<u8>()
                        .ok()
                        .filter(|n| (1..=6).contains(n))
                        .ok_or_else(|| format!("--string: '{s}' must be 1..=6"))?;
                    o.string = Some(n);
                }
                "--speed" => o.speed = Some(pos_num("--speed", val("--speed")?)?),
                other => return Err(format!("unknown option '{other}'")),
            }
        }
        if o.speed.is_some() && !o.fake {
            return Err("--speed needs --fake".into());
        }
        if o.cents.is_some() && !o.fake {
            return Err("--cents needs --fake".into());
        }
        if o.string.is_some() && !o.fake {
            return Err("--string needs --fake".into());
        }
        Ok(o)
    }

    fn open_request(&self) -> OpenRequest {
        OpenRequest {
            input_id: self.in_id.clone(),
            output_id: self.out_id.clone(),
            buffer_frames: None,
        }
    }
}

// ------------------------------------------------------------------ errors and exit codes

fn report_error(o: &Opts, code: i32, tech: &str, player: &str) -> i32 {
    if o.json {
        println!("{}", json!({"ok": false, "error": tech, "exit": code}));
    } else {
        println!("{player}");
        if o.details {
            eprintln!("({tech}; exit {code})");
        }
    }
    code
}

fn audio_exit(e: &AudioError) -> i32 {
    match e {
        AudioError::NoDevice(what) => {
            let w = what.to_lowercase();
            if w.contains("output") && !w.contains("input") {
                11
            } else {
                10
            }
        }
        AudioError::UnsupportedRate(_) | AudioError::UnsupportedFormat(_) => 13,
        AudioError::Backend(msg) => {
            let m = msg.to_lowercase();
            if m.contains("permission")
                || m.contains("denied")
                || m.contains("stream")
                || m.contains("access")
            {
                12
            } else {
                20
            }
        }
    }
}

fn audio_error(o: &Opts, e: &AudioError) -> i32 {
    let code = audio_exit(e);
    let player = match code {
        10 => "Não achei um microfone. Conecte um e tente de novo quando quiser.",
        11 => "Não achei uma saída de som. Conecte uma e tente de novo quando quiser.",
        12 => "Não consegui abrir o som. Veja se o app tem permissão para o microfone.",
        13 => "Esse dispositivo usa um formato de som que ainda não conheço.",
        _ => "Não deu para abrir o som agora. Tente de novo quando quiser.",
    };
    report_error(o, code, &e.to_string(), player)
}

// ------------------------------------------------------------------ note helpers

fn note_name(midi: i32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[midi.rem_euclid(12) as usize],
        midi.div_euclid(12) - 1
    )
}

fn freq_to_midi_cents(freq: f64, a4: f64) -> Option<(i32, f64)> {
    if freq <= 0.0 || !freq.is_finite() {
        return None;
    }
    let mf = 69.0 + 12.0 * (freq / a4).ln() / std::f64::consts::LN_2;
    let r = (mf + 0.5).floor();
    let cents = (mf - r) * 100.0;
    let ri = r as i32;
    (38..=91).contains(&ri).then_some((ri, cents))
}

/// Short, encouraging pt-BR guidance for a tuning offset in cents (no red, no failure wording).
fn tuning_hint_pt(cents: f64) -> &'static str {
    if cents.abs() <= IN_TUNE_CENTS {
        "afinada"
    } else if cents < 0.0 {
        "um pouco grave"
    } else {
        "um pouco aguda"
    }
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    })
}

/// Accumulates confirmed frames of one sounding note and summarises them via median (skipping
/// the initial pluck transient frames once settled frames exist).
struct NoteCapture {
    tracker: Tracker,
    a4: f64,
    active_midi: Option<i32>,
    samples: Vec<(f64, f64, f64)>, // (freq_hz, cents, clarity)
    finished: Vec<Reading>,
}

#[derive(Clone, Debug)]
struct Reading {
    midi: i32,
    cents: f64,
    freq_hz: f64,
    clarity: f64,
}

impl NoteCapture {
    fn new(fs_in: f64, a4: f64) -> Self {
        let cfg = FrontendConfig::demo(fs_in);
        let hop_s = cfg.hop as f64 * cfg.d as f64 / fs_in;
        Self {
            tracker: Tracker::new(TrackerParams::for_hop(hop_s)),
            a4,
            active_midi: None,
            samples: Vec::new(),
            finished: Vec::new(),
        }
    }

    fn push_frame(&mut self, fr: &tonica_dsp::Frame) {
        let prev = self.tracker.current();
        let mut new_attack_note = false;
        self.tracker.push(fr, |ev| {
            if let Event::Note { attack: true, .. } = ev {
                new_attack_note = true;
            }
        });
        let cur = self.tracker.current();
        if new_attack_note || (prev.is_some() && cur != prev) {
            self.flush();
        }
        if let Some(t_midi) = cur {
            let (midi, cents) = if (self.a4 - 440.0).abs() < 1e-9 {
                (t_midi, self.tracker.cents())
            } else if let Some(pair) = freq_to_midi_cents(fr.f, self.a4) {
                pair
            } else {
                return;
            };
            if fr.rms > self.tracker.gate() && fr.f > 0.0 && fr.c >= MIN_CLARITY {
                self.active_midi = Some(midi);
                self.samples.push((fr.f, cents, fr.c));
            }
        }
    }

    fn flush(&mut self) {
        if let Some(midi) = self.active_midi.take()
            && !self.samples.is_empty()
        {
            // Skip the earliest transient frames when enough settled frames exist.
            let skip = if self.samples.len() > 8 { 3 } else { 0 };
            let slice = &self.samples[skip..];
            let mut freqs: Vec<f64> = slice.iter().map(|s| s.0).collect();
            let mut cents: Vec<f64> = slice.iter().map(|s| s.1).collect();
            let mut clars: Vec<f64> = slice.iter().map(|s| s.2).collect();
            if let (Some(freq_hz), Some(cents), Some(clarity)) =
                (median(&mut freqs), median(&mut cents), median(&mut clars))
            {
                self.finished.push(Reading {
                    midi,
                    cents,
                    freq_hz,
                    clarity,
                });
            }
        }
        self.samples.clear();
    }
}

// ------------------------------------------------------------------ fake mode

#[derive(Clone, Debug)]
struct FakeCase {
    string_num: u8,
    expected_midi: i32,
    expected_cents: f64,
    expected_freq_hz: f64,
}

fn fake_cases(o: &Opts) -> Vec<FakeCase> {
    // Guitar strings: 6 = E2 (OPEN[0] = 40) .. 1 = E4 (OPEN[5] = 64).
    let strings: Vec<u8> = match o.string {
        Some(n) => vec![n],
        None => vec![6, 5, 4, 3, 2, 1],
    };
    let offsets: Vec<f64> = match o.cents {
        Some(c) => vec![c],
        None => T13_CENTS.to_vec(),
    };
    let mut out = Vec::with_capacity(strings.len() * offsets.len());
    for s in strings {
        let idx = (6 - s) as usize;
        let midi = OPEN[idx];
        for &c in &offsets {
            let freq = mtof(f64::from(midi) + c / 100.0, o.a4);
            out.push(FakeCase {
                string_num: s,
                expected_midi: midi,
                expected_cents: c,
                expected_freq_hz: freq,
            });
        }
    }
    out
}

fn run_fake(o: &Opts) -> i32 {
    let cases = fake_cases(o);
    let mut fc = FakeConfig::default();
    fc.path.noise_rms = FAKE_NOISE;
    let backend = FakeBackend::new(fc);
    let handle = backend.handle();
    let (mut eng, mut io) = match AudioEngine::start(
        backend,
        EngineConfig::default(),
        SoundBank::new(48_000.0),
        Pump::Manual,
    ) {
        Ok(x) => x,
        Err(e) => return audio_error(o, &e),
    };

    let fs_in = f64::from(eng.info().fs_in);
    let mut capture = NoteCapture::new(fs_in, o.a4);
    let mut rng = Mulberry32::new(13);
    let ring_samples = (RING_S * fs_in).round() as usize;
    let total_samples = ((RING_S + 0.02) * fs_in).round() as usize;

    // Warm up the noise floor briefly before the first pluck.
    let warmup_s = 0.05;
    let steps = (warmup_s / SLICE_S).round() as usize;
    for _ in 0..steps {
        handle.pump(SLICE_S);
        eng.pump();
        drain_pitch(&mut io, &mut capture);
    }

    let mut results = Vec::with_capacity(cases.len());
    for case in &cases {
        let p = Pluck::new(case.expected_freq_hz);
        let samples: Arc<[f32]> = Arc::from(render(
            fs_in,
            &p,
            total_samples,
            Some(ring_samples),
            &[],
            &mut rng,
        ));
        let start_at = handle.now_s() + 0.01;
        let queued = handle.play_external(start_at, samples, FAKE_GAIN);
        if queued.is_none() {
            eng.stop();
            return report_error(
                o,
                20,
                "fake handle refused synthetic string",
                "Não foi possível preparar a corda sintética agora.",
            );
        }
        let slot_steps = (SLOT_S / SLICE_S).round() as usize;
        let before = capture.finished.len();
        for _ in 0..slot_steps {
            handle.pump(SLICE_S);
            eng.pump();
            drain_pitch(&mut io, &mut capture);
            if let Some(s) = o.speed {
                std::thread::sleep(Duration::from_secs_f64(SLICE_S / s));
            }
        }
        capture.flush();
        let reading = capture.finished.get(before).cloned();
        let error_cents = reading.as_ref().map(|r| {
            let semitone_diff = f64::from(r.midi - case.expected_midi) * 100.0;
            (r.cents + semitone_diff - case.expected_cents).abs()
        });
        results.push((case.clone(), reading, error_cents));
    }
    eng.stop();

    let max_error = results
        .iter()
        .filter_map(|(_, _, e)| *e)
        .fold(0.0f64, f64::max);
    let all_heard = results.iter().all(|(_, r, _)| r.is_some());

    if o.json {
        let notes: Vec<Value> = results
            .iter()
            .map(|(case, reading, err)| {
                let note = reading
                    .as_ref()
                    .map(|r| note_name(r.midi))
                    .unwrap_or_else(|| note_name(case.expected_midi));
                json!({
                    "string": case.string_num,
                    "note": note,
                    "midi": reading.as_ref().map(|r| r.midi),
                    "expected_note": note_name(case.expected_midi),
                    "expected_midi": case.expected_midi,
                    "expected_cents": case.expected_cents,
                    "cents": reading.as_ref().map(|r| r.cents),
                    "freq_hz": reading.as_ref().map(|r| r.freq_hz),
                    "expected_freq_hz": case.expected_freq_hz,
                    "clarity": reading.as_ref().map(|r| r.clarity),
                    "error_cents": err,
                })
            })
            .collect();
        println!(
            "{}",
            json!({
                "ok": all_heard,
                "exit": 0,
                "fake": true,
                "a4_hz": o.a4,
                "fs_in": fs_in,
                "notes": notes,
                "max_error_cents": all_heard.then_some(max_error),
            })
        );
        return 0;
    }

    for (case, reading, err) in &results {
        match reading {
            Some(r) => {
                println!(
                    "Corda {} ({}): {:+5.1} cents ({})",
                    case.string_num,
                    note_name(r.midi),
                    r.cents,
                    tuning_hint_pt(r.cents),
                );
                if o.details {
                    println!(
                        "  medida {:.2} Hz (esperado {:.2} Hz, diferença {:.2} cents, clareza {:.3})",
                        r.freq_hz,
                        case.expected_freq_hz,
                        err.unwrap_or(0.0),
                        r.clarity,
                    );
                }
            }
            None => {
                println!(
                    "Corda {} ({}): ainda sem leitura firme",
                    case.string_num,
                    note_name(case.expected_midi),
                );
            }
        }
    }
    if o.details && all_heard {
        println!("diferença máxima: {max_error:.2} cents");
    }
    0
}

fn drain_pitch(io: &mut EngineIo, capture: &mut NoteCapture) -> bool {
    let mut stopped = false;
    while let Ok(ev) = io.events.pop() {
        match ev {
            EngineEvent::Pitch(p) => capture.push_frame(&p.frame),
            EngineEvent::Stopped(_) => stopped = true,
            _ => {}
        }
    }
    stopped
}

fn print_live_notes(capture: &NoteCapture, shown: &mut usize, details: bool) {
    while *shown < capture.finished.len() {
        let r = &capture.finished[*shown];
        println!(
            "{}: {:+5.1} cents ({})",
            note_name(r.midi),
            r.cents,
            tuning_hint_pt(r.cents),
        );
        if details {
            println!("  {:.2} Hz (clareza {:.3})", r.freq_hz, r.clarity);
        }
        *shown += 1;
    }
}

// ------------------------------------------------------------------ live mode

fn run_live(o: &Opts) -> i32 {
    let ec = EngineConfig {
        open: o.open_request(),
        ..EngineConfig::default()
    };
    let (mut eng, mut io) = match AudioEngine::start(
        CpalBackend::new(),
        ec,
        SoundBank::new(48_000.0),
        Pump::Thread,
    ) {
        Ok(x) => x,
        Err(e) => return audio_error(o, &e),
    };
    let fs_in = f64::from(eng.info().fs_in);
    let mut capture = NoteCapture::new(fs_in, o.a4);
    let start = Instant::now();
    let mut shown = 0usize;
    let mut stopped = false;
    if !o.json {
        println!("Escutando o violão...");
    }
    while start.elapsed().as_secs_f64() < o.seconds {
        let alive = eng.poll();
        stopped |= drain_pitch(&mut io, &mut capture);
        if !o.json {
            print_live_notes(&capture, &mut shown, o.details);
        }
        if !alive || stopped {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let stats = eng.stop();
    stopped |= drain_pitch(&mut io, &mut capture);
    capture.flush();

    let streams_ok = stats.callbacks_in > 0 && !stats.device_lost && !stopped;
    let exit = if streams_ok { 0 } else { 12 };

    if o.json {
        let notes: Vec<Value> = capture
            .finished
            .iter()
            .map(|r| {
                json!({
                    "note": note_name(r.midi),
                    "midi": r.midi,
                    "cents": r.cents,
                    "freq_hz": r.freq_hz,
                    "clarity": r.clarity,
                })
            })
            .collect();
        println!(
            "{}",
            json!({
                "ok": streams_ok,
                "exit": exit,
                "fake": false,
                "a4_hz": o.a4,
                "fs_in": fs_in,
                "seconds": o.seconds,
                "notes": notes,
            })
        );
        return exit;
    }

    print_live_notes(&capture, &mut shown, o.details);
    if !streams_ok {
        println!(
            "O som parou ou o microfone ainda não está respondendo. Quando quiser, tente de novo."
        );
    } else if capture.finished.is_empty() {
        println!("Nenhuma nota ouvida ainda. Toque uma corda quando quiser.");
    }
    exit
}
