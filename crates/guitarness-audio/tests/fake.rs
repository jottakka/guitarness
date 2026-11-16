//! The fake backend is the oracle of milestone M2, so it is tested against first
//! principles: the delay of the virtual acoustic path (impulse centroid and matched-filter
//! loopback), the stamps against the configured drift and jitter, xrun and device-change
//! injection, determinism, allocation, and the `ClockMap` fed by the fake (the meeting
//! point of units U3 and U5). Run with `--nocapture` to see the measured numbers.

use std::sync::{Arc, Mutex, MutexGuard};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use guitarness_audio::clock::{ClockConfig, ClockMap, ClockQuality, ClockStep, StepCause};
use guitarness_audio::fake::{
    BlockModel, FAKE_INPUT_ID, FAKE_OUTPUT_ID, FakeBackend, FakeConfig, FakeHandle, FakeStreams,
    FakeTruth,
};
use guitarness_audio::{
    AudioError, Backend, BackendEvent, HostClock, IoCallbacks, OpenRequest, Side, Stamp,
    StreamInfo, XrunBoundary,
};
use guitarness_dsp::latency::{LoopbackConfig, Probe, measure_loopback};
use guitarness_dsp::synth::Mulberry32;

#[global_allocator]
static A: AllocDisabler = AllocDisabler;

/// A sound the output callback plays at an exact output frame.
struct Play {
    at: u64,
    samples: Arc<[f32]>,
    gain: f32,
}

/// One `ClockMap` reading, taken in the output callback.
#[derive(Clone, Copy)]
struct Mark {
    frame: u64,
    in_of_out: f64,
    drift_ppm: f64,
    quality: ClockQuality,
}

/// One call of the error callback.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Notice {
    ev: BackendEvent,
    /// The xrun boundary the backend gave.
    at: Option<u64>,
    /// Input and output blocks that had run when it was delivered.
    blocks: (usize, usize),
}

/// What the callbacks do and record. Everything is preallocated: the callbacks only
/// push within capacity.
struct Tape {
    ch_in: usize,
    ch_out: usize,
    rec: Vec<f32>,
    in_stamps: Vec<Stamp>,
    out_stamps: Vec<Stamp>,
    events: Vec<BackendEvent>,
    notices: Vec<Notice>,
    plays: Vec<Play>,
    /// Constant level on every output channel.
    dc: f32,
    /// `false`: the output callback leaves the buffer untouched.
    write_output: bool,
    /// Play only on channel 0.
    left_only: bool,
    channels_differ: bool,
    clock: Option<ClockMap>,
    forward_xruns: bool,
    xrun_seen: [bool; 2],
    marks: Vec<Mark>,
    /// Steps of the clock map, with the number of stamps of that side fed so far.
    steps: Vec<(ClockStep, usize)>,
}

fn on_output(t: &mut Tape, buf: &mut [f32], s: Stamp) {
    t.out_stamps.push(s);
    if t.write_output {
        buf.fill(t.dc);
        let (b0, b1) = (s.first, s.first + u64::from(s.frames));
        for p in &t.plays {
            let p1 = p.at + p.samples.len() as u64;
            for f in p.at.max(b0)..p1.min(b1) {
                let v = p.gain * p.samples[(f - p.at) as usize];
                let i = (f - b0) as usize * t.ch_out;
                buf[i] += v;
                if !t.left_only {
                    for c in 1..t.ch_out {
                        buf[i + c] += v;
                    }
                }
            }
        }
    }
    if let Some(map) = t.clock.as_mut() {
        if t.forward_xruns && std::mem::take(&mut t.xrun_seen[1]) {
            map.note_xrun(Side::Out);
        }
        if let Some(step) = map.push_out(s) {
            t.steps.push((step, t.out_stamps.len()));
        }
        let snap = map.snapshot();
        t.marks.push(Mark {
            frame: s.first,
            in_of_out: snap.in_of_out(s.first as f64),
            drift_ppm: snap.drift_ppm(),
            quality: snap.quality(),
        });
    }
}

fn on_input(t: &mut Tape, buf: &[f32], s: Stamp) {
    t.in_stamps.push(s);
    for frame in buf.chunks_exact(t.ch_in) {
        t.channels_differ |= frame.iter().any(|v| v.to_bits() != frame[0].to_bits());
        t.rec.push(frame[0]);
    }
    if let Some(map) = t.clock.as_mut() {
        if t.forward_xruns && std::mem::take(&mut t.xrun_seen[0]) {
            map.note_xrun(Side::In);
        }
        if let Some(step) = map.push_in(s) {
            t.steps.push((step, t.in_stamps.len()));
        }
    }
}

fn on_error(t: &mut Tape, ev: BackendEvent, at: Option<XrunBoundary>) {
    t.events.push(ev);
    // The fake knows every boundary: it never reports a lost one.
    let at = at.map(|b| match b {
        XrunBoundary::Exact(f) => f,
        XrunBoundary::Within { .. } => panic!("the fake lost a boundary: {b:?}"),
    });
    t.notices.push(Notice {
        ev,
        at,
        blocks: (t.in_stamps.len(), t.out_stamps.len()),
    });
    match ev {
        BackendEvent::XrunIn => t.xrun_seen[0] = true,
        BackendEvent::XrunOut => t.xrun_seen[1] = true,
        _ => {}
    }
}

fn callbacks(tape: &Arc<Mutex<Tape>>) -> IoCallbacks {
    let (a, b, c) = (Arc::clone(tape), Arc::clone(tape), Arc::clone(tape));
    IoCallbacks {
        input: Box::new(move |buf, s| on_input(&mut a.lock().unwrap(), buf, s)),
        output: Box::new(move |buf, s| on_output(&mut b.lock().unwrap(), buf, s)),
        error: Box::new(move |ev, at| on_error(&mut c.lock().unwrap(), ev, at)),
    }
}

struct Rig {
    backend: FakeBackend,
    handle: FakeHandle,
    tape: Arc<Mutex<Tape>>,
    streams: Option<FakeStreams>,
    info: StreamInfo,
    /// The input stamps are on the raw input clock: the client adds the offset.
    raw_stamps: bool,
}

impl Rig {
    /// Opens the fake with room to record `seconds`.
    fn open(cfg: FakeConfig, seconds: f64) -> Self {
        Self::open_with(cfg, seconds, |_| {})
    }

    fn open_with(cfg: FakeConfig, seconds: f64, setup: impl FnOnce(&mut Tape)) -> Self {
        let blocks = (seconds * 1000.0) as usize + 1024;
        let mut tape = Tape {
            ch_in: usize::from(cfg.ch_in),
            ch_out: usize::from(cfg.ch_out),
            rec: Vec::with_capacity(((seconds + 1.0) * f64::from(cfg.fs_in) * 1.01) as usize),
            in_stamps: Vec::with_capacity(blocks),
            out_stamps: Vec::with_capacity(blocks),
            events: Vec::with_capacity(64),
            notices: Vec::with_capacity(64),
            plays: Vec::new(),
            dc: 0.0,
            write_output: true,
            left_only: false,
            channels_differ: false,
            clock: None,
            forward_xruns: false,
            xrun_seen: [false; 2],
            marks: Vec::with_capacity(blocks),
            steps: Vec::with_capacity(64),
        };
        setup(&mut tape);
        let tape = Arc::new(Mutex::new(tape));
        let raw_stamps = cfg.raw_input_stamps;
        let backend = FakeBackend::new(cfg);
        let handle = backend.handle();
        let (streams, info) = backend
            .open(&OpenRequest::default(), callbacks(&tape))
            .expect("open");
        Self {
            backend,
            handle,
            tape,
            streams: Some(streams),
            info,
            raw_stamps,
        }
    }

    /// What a client adds to the input stamps: nothing, unless they are raw.
    fn input_shift_ns(&self) -> i64 {
        if self.raw_stamps {
            self.info.origin_offset_ns
        } else {
            0
        }
    }

    fn tape(&self) -> MutexGuard<'_, Tape> {
        self.tape.lock().unwrap()
    }

    fn truth(&self) -> FakeTruth {
        self.handle.truth().expect("streams are open")
    }

    fn clock_config(&self) -> ClockConfig {
        ClockConfig {
            origin_offset_ns: self.input_shift_ns(),
            ..ClockConfig::default()
        }
    }

    /// The path never read output that was not there.
    fn assert_path_sound(&self) {
        let st = self.handle.stats();
        assert_eq!((st.path_starved, st.path_overrun), (0, 0), "{st:?}");
        assert_eq!(st.nonfinite_out, 0, "{st:?}");
    }
}

/// Least squares `y = a + b·x`: `(b, a, residual rms)`.
fn fit(x: &[f64], y: &[f64]) -> (f64, f64, f64) {
    let n = x.len() as f64;
    let (mx, my) = (x.iter().sum::<f64>() / n, y.iter().sum::<f64>() / n);
    let sxx: f64 = x.iter().map(|v| (v - mx) * (v - mx)).sum();
    let sxy: f64 = x.iter().zip(y).map(|(u, v)| (u - mx) * (v - my)).sum();
    let b = sxy / sxx;
    let a = my - b * mx;
    let ss: f64 = x.iter().zip(y).map(|(u, v)| (v - a - b * u).powi(2)).sum();
    (b, a, (ss / n).sqrt())
}

fn p95(v: &[f64]) -> f64 {
    assert!(!v.is_empty());
    let mut a: Vec<f64> = v.iter().map(|x| x.abs()).collect();
    a.sort_by(f64::total_cmp);
    a[((0.95 * a.len() as f64).ceil() as usize).clamp(1, a.len()) - 1]
}

fn varied(fs: u32) -> BlockModel {
    // 5 to 15 ms, like the clock tests.
    BlockModel::Uniform {
        min: fs / 200,
        max: fs * 3 / 200,
    }
}

/// (fs_in, fs_out, drift_in ppm, drift_out ppm, latency ms)
const PATHS: [(u32, u32, f64, f64, f64); 7] = [
    (48_000, 48_000, 0.0, 0.0, 35.0),
    (48_000, 48_000, 120.0, 0.0, 87.3),
    (48_000, 48_000, -80.0, 60.0, 142.6),
    (44_100, 48_000, 0.0, 0.0, 35.0),
    (44_100, 48_000, 120.0, -80.0, 87.3),
    (44_100, 48_000, -80.0, 200.0, 142.6),
    (48_000, 44_100, 200.0, -200.0, 87.3),
];

fn path_config(case: (u32, u32, f64, f64, f64), seed: u32) -> FakeConfig {
    let (fs_in, fs_out, drift_in, drift_out, latency_ms) = case;
    let mut cfg = FakeConfig::new(fs_in, fs_out);
    cfg.drift_in_ppm = drift_in;
    cfg.drift_out_ppm = drift_out;
    cfg.blocks_in = varied(fs_in);
    cfg.blocks_out = varied(fs_out);
    cfg.stamp_jitter_in_s = 50.0e-6;
    cfg.stamp_jitter_out_s = 50.0e-6;
    cfg.path.latency_s = latency_ms * 1.0e-3;
    cfg.seed = seed;
    cfg
}

// ---------------------------------------------------------------- open and devices

#[test]
fn devices_open_and_errors() {
    let backend = FakeBackend::new(FakeConfig::new(44_100, 48_000));
    let devices = backend.devices().unwrap();
    assert_eq!(devices.len(), 2);
    assert!(
        devices
            .iter()
            .any(|d| d.id == FAKE_INPUT_ID && d.is_input && d.is_default)
    );
    assert!(
        devices
            .iter()
            .any(|d| d.id == FAKE_OUTPUT_ID && d.is_output && d.is_default)
    );

    let tape = Rig::open(FakeConfig::default(), 0.1).tape.clone();
    let wrong = OpenRequest {
        input_id: Some("usb-mic".into()),
        ..OpenRequest::default()
    };
    assert!(matches!(
        backend.open(&wrong, callbacks(&tape)),
        Err(AudioError::NoDevice(_))
    ));
    let named = OpenRequest {
        input_id: Some(FAKE_INPUT_ID.into()),
        output_id: Some(FAKE_OUTPUT_ID.into()),
        buffer_frames: Some(256),
    };
    let (streams, info) = backend.open(&named, callbacks(&tape)).unwrap();
    assert_eq!(
        (info.fs_in, info.fs_out, info.ch_in, info.ch_out),
        (44_100, 48_000, 1, 2)
    );
    assert_eq!(
        (info.in_id.as_str(), info.out_id.as_str()),
        (FAKE_INPUT_ID, FAKE_OUTPUT_ID)
    );
    assert_eq!(info.origin_offset_ns, 0);
    assert!(matches!(
        backend.open(&OpenRequest::default(), callbacks(&tape)),
        Err(AudioError::Backend(_))
    ));
    // The requested buffer size is used for every block of both streams.
    let handle = backend.handle();
    assert!(handle.pump(0.25) > 40);
    {
        let t = tape.lock().unwrap();
        assert!(
            t.in_stamps
                .iter()
                .chain(&t.out_stamps)
                .all(|s| s.frames == 256)
        );
    }
    drop(streams);
    assert!(!handle.is_open());
    assert_eq!(handle.pump(0.1), 0, "closed streams run no callback");

    for (fs_in, fs_out) in [(16_000, 48_000), (48_000, 96_000)] {
        let b = FakeBackend::new(FakeConfig::new(fs_in, fs_out));
        let bad = if fs_in == 16_000 { fs_in } else { fs_out };
        assert_eq!(
            b.open(&OpenRequest::default(), callbacks(&tape)).err(),
            Some(AudioError::UnsupportedRate(bad))
        );
    }
    let b = FakeBackend::new(FakeConfig {
        blocks_in: BlockModel::Cycle(vec![]),
        ..FakeConfig::default()
    });
    assert!(matches!(
        b.open(&OpenRequest::default(), callbacks(&tape)),
        Err(AudioError::Backend(_))
    ));
}

// ---------------------------------------------------------------- the acoustic path

/// Centre of mass of the recording around `near` (± `half` frames), and its sum.
fn centroid(rec: &[f32], near: f64, half: usize) -> (f64, f64) {
    let mid = near.round() as usize;
    let (mut sum, mut moment) = (0.0f64, 0.0f64);
    for (g, v) in rec.iter().enumerate().take(mid + half + 1).skip(mid - half) {
        sum += f64::from(*v);
        moment += g as f64 * f64::from(*v);
    }
    (moment / sum, sum)
}

#[test]
fn an_impulse_arrives_at_the_true_delay() {
    // A band-limited pulse has its centre of mass exactly at its true position, so this
    // measures the delay of the path with no estimator in between.
    let mut worst_frames: f64 = 0.0;
    for case in PATHS {
        let mut cfg = path_config(case, 11);
        cfg.path.gain = 0.8;
        let impulse: Arc<[f32]> = Arc::from(vec![1.0f32]);
        let at: Vec<u64> = (0..8).map(|k| 9_000 + 7_001 * k).collect();
        let rig = Rig::open_with(cfg, 2.0, |t| {
            for a in &at {
                t.plays.push(Play {
                    at: *a,
                    samples: Arc::clone(&impulse),
                    gain: 0.5,
                });
            }
        });
        rig.handle.pump(1.8);
        rig.assert_path_sound();
        let truth = rig.truth();
        let tape = rig.tape();
        for a in &at {
            let expect = truth.arrival_in(*a as f64);
            let (centre, sum) = centroid(&tape.rec, expect, 48);
            let err = centre - expect;
            worst_frames = worst_frames.max(err.abs());
            assert!(
                err.abs() < 0.01,
                "{case:?}: pulse at {centre:.4}, expected {expect:.4}"
            );
            // Area: amplitude × gain × rate ratio (the pulse is one output frame wide).
            let area = 0.5 * 0.8 * truth.rate_in() / truth.rate_out();
            assert!(
                (sum / area - 1.0).abs() < 2.0e-3,
                "{case:?}: area {sum} vs {area}"
            );
            // And it is the configured latency after the instant the frame played.
            let delay_ms = (truth.in_time_s(centre) - truth.out_time_s(*a as f64)) * 1.0e3;
            assert!((delay_ms - case.4).abs() < 0.001, "{case:?}: {delay_ms} ms");
        }
    }
    println!(
        "impulse centroid vs true arrival: worst {worst_frames:.5} frames ({:.5} ms at 44.1 kHz) over 7 configurations x 8 pulses",
        worst_frames / 44.1
    );
}

#[test]
fn the_stamps_alone_give_the_latency() {
    // No oracle here: only what a backend client sees. The play time of a frame comes
    // from the output stamps, the capture time of its echo from the input stamps; the
    // difference is the configured latency. This ties the stamps to the audio.
    // Default: the stamps of both streams are on one clock, as from the cpal backend,
    // and the reported origin offset must not be added. Raw: the client adds it.
    let (rate_in, rate_out) = (44_100.0 * (1.0 + 120.0e-6), 48_000.0 * (1.0 - 80.0e-6));
    let impulse: Arc<[f32]> = Arc::from(vec![1.0f32]);
    let at: Vec<u64> = (0..8).map(|k| 9_000 + 7_001 * k).collect();
    let time_of = |stamps: &[Stamp], frame: f64, rate: f64, shift_ns: i64| {
        let s = stamps
            .iter()
            .rev()
            .find(|s| s.first as f64 <= frame)
            .expect("a block holds the frame");
        (s.device_ns as i64 + shift_ns) as f64 * 1.0e-9 + (frame - s.first as f64) / rate
    };
    for raw in [false, true] {
        let mut cfg = path_config(PATHS[4], 21);
        cfg.stamp_jitter_in_s = 0.0;
        cfg.stamp_jitter_out_s = 0.0;
        cfg.origin_offset_ns = 1_234_567;
        cfg.raw_input_stamps = raw;
        cfg.path.gain = 0.8;
        let rig = Rig::open_with(cfg, 2.0, |t| {
            for a in &at {
                t.plays.push(Play {
                    at: *a,
                    samples: Arc::clone(&impulse),
                    gain: 0.5,
                });
            }
        });
        assert_eq!(rig.info.origin_offset_ns, 1_234_567);
        assert_eq!(rig.input_shift_ns(), if raw { 1_234_567 } else { 0 });
        rig.handle.pump(1.8);
        let tape = rig.tape();
        // The pulses are far apart: each is the largest sample of its neighbourhood.
        let peaks: Vec<usize> = (64..tape.rec.len() - 64)
            .filter(|g| {
                tape.rec[*g] > 0.1 && tape.rec[g - 40..g + 40].iter().all(|v| *v <= tape.rec[*g])
            })
            .collect();
        assert_eq!(peaks.len(), at.len());
        let (mut worst, mut worst_wrong): (f64, f64) = (0.0, f64::MAX);
        for (a, peak) in at.iter().zip(&peaks) {
            let (centre, _) = centroid(&tape.rec, *peak as f64, 48);
            let played = time_of(&tape.out_stamps, *a as f64, rate_out, 0);
            let heard = time_of(&tape.in_stamps, centre, rate_in, rig.input_shift_ns());
            let err = heard - played - 0.0873;
            worst = worst.max(err.abs());
            assert!(err.abs() < 1.0e-6, "latency from the stamps off by {err} s");
            // The other convention is off by the whole offset: the check can fail.
            let wrong = if raw { 0 } else { rig.info.origin_offset_ns };
            let heard = time_of(&tape.in_stamps, centre, rate_in, wrong);
            worst_wrong = worst_wrong.min((heard - played - 0.0873).abs());
        }
        assert!(
            worst_wrong > 1.2e-3,
            "wrong convention off by {worst_wrong} s"
        );
        println!(
            "latency from stamps and samples only (raw input stamps: {raw}): worst error {:.1} ns; wrong convention off by {:.3} ms",
            worst * 1.0e9,
            worst_wrong * 1.0e3
        );
    }
}

fn probe_run(case: (u32, u32, f64, f64, f64), seed: u32) -> (Rig, Vec<u64>) {
    let fs_out = case.1;
    let mut cfg = path_config(case, seed);
    cfg.path.gain = 0.3;
    cfg.path.noise_rms = 0.003;
    let probe: Arc<[f32]> = Arc::from(Probe::new(f64::from(fs_out)).signal());
    let mut rng = Mulberry32::new(seed);
    // Ten probes, 450 ms ± 40 ms apart, as the calibration will play them.
    let mut at = Vec::new();
    let mut t = 0.4;
    for _ in 0..10 {
        at.push((t * f64::from(fs_out)).round() as u64);
        t += 0.450 + 0.080 * (rng.next_f64() - 0.5);
    }
    let rig = Rig::open_with(cfg, 6.0, |tape| {
        for a in &at {
            tape.plays.push(Play {
                at: *a,
                samples: Arc::clone(&probe),
                gain: 0.25,
            });
        }
    });
    rig.handle.pump(5.6);
    rig.assert_path_sound();
    (rig, at)
}

#[test]
fn loopback_measures_the_configured_latency_within_0_05_ms() {
    let mut worst: f64 = 0.0;
    for (k, case) in PATHS.into_iter().enumerate() {
        let (rig, at) = probe_run(case, 40 + k as u32);
        let truth = rig.truth();
        let tape = rig.tape();
        // Emission of each probe on the input clock: the frame captured as it played.
        let emits: Vec<f64> = at.iter().map(|a| truth.in_of_out(*a as f64)).collect();
        let probe = Probe::new(f64::from(case.0));
        let lb = measure_loopback(&tape.rec, &emits, &probe, &LoopbackConfig::default())
            .unwrap_or_else(|e| panic!("{case:?}: {e}"));
        assert_eq!(lb.found, 10, "{case:?}");
        let ms = lb.latency / truth.rate_in() * 1.0e3;
        let err = ms - case.4;
        let spread_ms = lb.mad / truth.rate_in() * 1.0e3;
        println!(
            "loopback {}/{} Hz, drift {:+.0}/{:+.0} ppm, L = {:.1} ms: measured {ms:.4} ms, error {err:+.4} ms, MAD {spread_ms:.4} ms",
            case.0, case.1, case.2, case.3, case.4
        );
        assert!(err.abs() <= 0.05, "{case:?}: {ms:.4} ms");
        assert!(spread_ms <= 0.01, "{case:?}: MAD {spread_ms} ms");
        worst = worst.max(err.abs());

        // Negative control: the same recording read against emissions three frames
        // late (0.06 ms) fails the gate, so the gate can tell.
        let late: Vec<f64> = emits.iter().map(|e| e + 3.0).collect();
        let lb = measure_loopback(&tape.rec, &late, &probe, &LoopbackConfig::default()).unwrap();
        let off = lb.latency / truth.rate_in() * 1.0e3 - case.4;
        assert!(off.abs() > 0.05, "{case:?}: control error {off} ms");
    }
    println!("loopback: worst latency error {worst:.4} ms (gate 0.05 ms)");
}

#[test]
fn impulse_response_gain_noise_and_silence() {
    // Direct sound plus a louder, inverted echo 1.5 ms later.
    let mut cfg = path_config(PATHS[4], 60);
    let mut ir = vec![0.0f32; 67];
    ir[0] = 0.5;
    ir[66] = -1.0;
    cfg.path.ir = Some(ir);
    cfg.path.gain = 0.6;
    let impulse: Arc<[f32]> = Arc::from(vec![1.0f32]);
    let rig = Rig::open_with(cfg, 3.0, |t| {
        for at in [20_000, 70_000] {
            t.plays.push(Play {
                at,
                samples: Arc::clone(&impulse),
                gain: 0.5,
            });
        }
    });
    rig.handle.pump(1.0);
    let truth = rig.truth();
    let area = 0.5 * 0.6 * truth.rate_in() / truth.rate_out();
    {
        let tape = rig.tape();
        let direct = truth.arrival_in(20_000.0);
        let (c0, s0) = centroid(&tape.rec, direct, 30);
        let (c1, s1) = centroid(&tape.rec, direct + 66.0, 30);
        assert!(
            (c0 - direct).abs() < 0.01 && (c1 - direct - 66.0).abs() < 0.01,
            "{c0} {c1}"
        );
        assert!((s0 / (0.5 * area) - 1.0).abs() < 5.0e-3, "direct {s0}");
        assert!((s1 / (-area) - 1.0).abs() < 5.0e-3, "echo {s1}");
    }
    // Gain 0 is silence; a new latency applies to what is heard from then on.
    rig.handle.set_path_gain(0.0);
    rig.handle.pump(0.2);
    let quiet_from = rig.tape().rec.len();
    rig.handle.pump(0.2);
    assert!(rig.tape().rec[quiet_from..].iter().all(|v| *v == 0.0));
    rig.handle.set_path_gain(0.6);
    rig.handle.set_latency_s(0.0200);
    rig.handle.pump(0.4);
    let truth = rig.truth();
    assert_eq!(truth.latency_s, 0.0200);
    let tape = rig.tape();
    let direct = truth.arrival_in(70_000.0);
    let (c, s) = centroid(&tape.rec, direct, 30);
    assert!(
        (c - direct).abs() < 0.01,
        "after set_latency_s: {c} vs {direct}"
    );
    assert!((s / (0.5 * area) - 1.0).abs() < 5.0e-3);
    drop(tape);
    rig.assert_path_sound();

    // Noise has the configured RMS and the same seed repeats it.
    let noise = |seed: u32| {
        let mut cfg = FakeConfig::new(48_000, 48_000);
        cfg.path.noise_rms = 0.01;
        cfg.seed = seed;
        let rig = Rig::open(cfg, 3.0);
        rig.handle.pump(2.0);
        std::mem::take(&mut rig.tape().rec)
    };
    let (a, b, c) = (noise(5), noise(5), noise(6));
    let rms = (a.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / a.len() as f64).sqrt();
    assert!((rms / 0.01 - 1.0).abs() < 0.02, "noise rms {rms}");
    assert!(a == b && a != c);
}

#[test]
fn channels_poison_and_clipping() {
    // Two input channels carry the same signal; the speaker plays the mean of the
    // output channels; the converters clip at ±1.
    let mut cfg = FakeConfig::new(48_000, 48_000);
    cfg.ch_in = 2;
    cfg.path.gain = 1.0;
    let rig = Rig::open_with(cfg.clone(), 2.0, |t| {
        t.dc = 0.5;
        t.left_only = true;
        t.plays.push(Play {
            at: 0,
            samples: Arc::from(vec![0.0f32]),
            gain: 0.0,
        });
    });
    rig.handle.pump(0.5);
    // dc on both channels: mean 0.5.
    assert!((rig.tape().rec.last().unwrap() - 0.5).abs() < 1.0e-4);
    assert!(!rig.tape().channels_differ);
    rig.tape().dc = 3.0; // clips to 1.0 at the converter
    rig.handle.pump(0.5);
    assert!((rig.tape().rec.last().unwrap() - 1.0).abs() < 1.0e-4);
    rig.assert_path_sound();
    drop(rig);

    // Sound on the left channel only reaches the microphone at half level.
    let rig = Rig::open_with(cfg.clone(), 2.0, |t| {
        t.left_only = true;
        t.plays.push(Play {
            at: 0,
            samples: Arc::from(vec![0.4f32; 48_000]),
            gain: 1.0,
        });
    });
    rig.handle.pump(0.5);
    assert!((rig.tape().rec.last().unwrap() - 0.2).abs() < 1.0e-4);
    drop(rig);

    // A callback that does not write: the poisoned buffer is counted and played as
    // silence. With the poison off the buffer arrives zeroed.
    for poison in [true, false] {
        let mut cfg = cfg.clone();
        cfg.poison_out = poison;
        let rig = Rig::open_with(cfg, 1.0, |t| t.write_output = false);
        rig.handle.pump(0.5);
        let st = rig.handle.stats();
        assert_eq!(st.nonfinite_out, if poison { st.frames_out } else { 0 });
        assert!(st.frames_out >= 24_000);
        assert!(rig.tape().rec.iter().all(|v| *v == 0.0));
    }
}

// ---------------------------------------------------------------- stamps

#[test]
fn stamps_follow_the_configured_drift_and_jitter() {
    let mut cfg = FakeConfig::new(44_100, 48_000);
    cfg.drift_in_ppm = 150.0;
    cfg.drift_out_ppm = -50.0;
    cfg.blocks_in = varied(44_100);
    cfg.blocks_out = BlockModel::Cycle(vec![480, 448, 512, 480, 1024, 64]);
    cfg.stamp_jitter_in_s = 50.0e-6;
    cfg.stamp_jitter_out_s = 200.0e-6;
    cfg.callback_jitter_s = 300.0e-6;
    cfg.origin_offset_ns = 5_000_000_000;
    cfg.raw_input_stamps = true;
    cfg.seed = 77;
    let (origin, offset) = (cfg.host_origin_ns as f64, cfg.origin_offset_ns as f64);
    let rig = Rig::open(cfg, 21.0);
    assert_eq!(rig.info.origin_offset_ns, 5_000_000_000);
    rig.handle.pump(20.0);
    let truth = rig.truth();
    let tape = rig.tape();
    let sides = [
        (Side::In, &tape.in_stamps, 44_100.0, 150.0, 50.0e-6, offset),
        (Side::Out, &tape.out_stamps, 48_000.0, -50.0, 200.0e-6, 0.0),
    ];
    for (side, stamps, fs, drift, jitter, shift) in sides {
        assert!(stamps.len() > 1500, "{side:?}: {} stamps", stamps.len());
        // Frame counters are contiguous from zero.
        assert_eq!(stamps[0].first, 0);
        assert!(
            stamps
                .windows(2)
                .all(|w| w[1].first == w[0].first + u64::from(w[0].frames))
        );
        // Callback times never go back.
        assert!(
            stamps
                .windows(2)
                .all(|w| w[1].callback_ns >= w[0].callback_ns)
        );
        let x: Vec<f64> = stamps.iter().map(|s| s.first as f64).collect();
        let y: Vec<f64> = stamps
            .iter()
            .map(|s| (s.device_ns as f64 - origin + shift) * 1.0e-9)
            .collect();
        let (slope, _, rms) = fit(&x, &y);
        let ppm = (1.0 / (slope * fs) - 1.0) * 1.0e6;
        println!(
            "{side:?} stamps: rate {ppm:+.2} ppm (configured {drift:+.0}), jitter {:.1} us (configured {:.0})",
            rms * 1e6,
            jitter * 1e6
        );
        assert!((ppm - drift).abs() < 3.0, "{side:?}: {ppm} ppm");
        assert!((rms / jitter - 1.0).abs() < 0.08, "{side:?}: jitter {rms}");
        // Against the truth: unbiased, and the callback is on the right side of it.
        let dev: Vec<f64> = stamps
            .iter()
            .map(|s| {
                let t = match side {
                    Side::In => truth.in_time_s(s.first as f64),
                    Side::Out => truth.out_time_s(s.first as f64),
                };
                (s.device_ns as f64 - origin + shift) * 1.0e-9 - t
            })
            .collect();
        let bias = dev.iter().sum::<f64>() / dev.len() as f64;
        assert!(
            bias.abs() < 4.0 * jitter / (dev.len() as f64).sqrt(),
            "{side:?}: bias {bias}"
        );
        for s in stamps.iter() {
            let lag = (s.callback_ns as f64 - origin + shift) * 1.0e-9;
            match side {
                // Requested 20 ms (less the scheduling delay, at most half) before it plays.
                Side::Out => {
                    let lead = truth.out_time_s(s.first as f64) - lag;
                    assert!((0.0099..=0.0201).contains(&lead), "lead {lead}");
                }
                // Delivered at least 2 ms after its last frame was captured.
                Side::In => {
                    let end = truth.in_time_s((s.first + u64::from(s.frames)) as f64);
                    assert!(
                        lag - end >= 0.002 - 1.0e-9 && lag - end < 0.02,
                        "lag {}",
                        lag - end
                    );
                }
            }
        }
    }
    assert!(
        (truth.drift_ppm() - 200.0).abs() < 0.02,
        "{}",
        truth.drift_ppm()
    );
}

#[test]
fn stamp_outlier_is_injected_once() {
    let mut cfg = FakeConfig::new(48_000, 48_000);
    cfg.stamp_jitter_in_s = 0.0;
    let rig = Rig::open(cfg, 1.0);
    rig.handle.pump(0.2);
    let n = rig.tape().in_stamps.len();
    rig.handle.inject_stamp_outlier(Side::In, 0.010);
    rig.handle.pump(0.2);
    let tape = rig.tape();
    let d: Vec<i64> = tape
        .in_stamps
        .windows(2)
        .map(|w| w[1].device_ns as i64 - w[0].device_ns as i64)
        .collect();
    assert_eq!(d[n - 1], 20_000_000, "the next stamp is 10 ms late");
    assert_eq!(d[n], 0, "and the one after is back on the line");
    assert!(
        d.iter()
            .enumerate()
            .all(|(k, v)| k == n - 1 || k == n || *v == 10_000_000)
    );
}

// ---------------------------------------------------------------- xruns

#[test]
fn input_xrun_drops_frames_and_reports() {
    let mut cfg = FakeConfig::new(44_100, 48_000);
    cfg.drift_in_ppm = 100.0;
    cfg.blocks_in = varied(44_100);
    cfg.stamp_jitter_in_s = 0.0;
    cfg.path.gain = 0.0;
    let origin = cfg.host_origin_ns as f64;
    let rig = Rig::open(cfg, 3.0);
    // The microphone hears a ramp of the device position: lost frames show.
    rig.handle.set_external_fn(Some(Box::new(|pos, block| {
        for (i, v) in block.iter_mut().enumerate() {
            *v += ((pos + i as u64) % 1024) as f32 / 1024.0;
        }
    })));
    rig.handle.pump(1.0);
    let (cut, blocks_before) = {
        let t = rig.tape();
        (t.rec.len(), t.in_stamps.len())
    };
    assert!(rig.handle.inject_xrun(Side::In, 960));
    rig.handle.pump(1.0);
    let truth = rig.truth();
    let tape = rig.tape();
    assert_eq!(tape.events, [BackendEvent::XrunIn]);
    assert_eq!(rig.handle.stats().xruns_in, 1);
    // The frame counter stays contiguous: the frames were never delivered.
    assert!(
        tape.in_stamps
            .windows(2)
            .all(|w| w[1].first == w[0].first + u64::from(w[0].frames))
    );
    assert_eq!(tape.in_stamps[blocks_before].first as usize, cut);
    // The samples skip exactly 960 device frames at the cut.
    let ramp = |pos: usize| (pos % 1024) as f32 / 1024.0;
    assert!(
        tape.rec[..cut]
            .iter()
            .enumerate()
            .all(|(g, v)| *v == ramp(g))
    );
    assert!(
        tape.rec[cut..]
            .iter()
            .enumerate()
            .all(|(g, v)| *v == ramp(cut + 960 + g))
    );
    assert!(tape.rec.len() > cut + 40_000);
    // The stamps jump by the same 960 frames of device time, and the truth follows.
    let rate = truth.rate_in();
    for (k, s) in tape.in_stamps.iter().enumerate() {
        let lost = if k >= blocks_before { 960.0 } else { 0.0 };
        let t = (s.device_ns as f64 - origin) * 1.0e-9;
        let expect = truth.in_time_s(0.0) + (s.first as f64 + lost) / rate;
        assert!((t - expect).abs() < 2.0e-9, "stamp {k}: {t} vs {expect}");
        assert!((truth.in_time_s(s.first as f64) - expect).abs() < 1.0e-12);
    }
    let gap = truth.in_time_s(cut as f64) - truth.in_time_s(cut as f64 - 1.0);
    assert!((gap - 961.0 / rate).abs() < 1.0e-12);
    // A time inside the hole maps to the first frame after it.
    assert_eq!(
        truth.in_frame_at(truth.in_time_s(cut as f64) - 0.005),
        cut as f64
    );
}

#[test]
fn output_xrun_plays_silence_and_reports() {
    for notify in [true, false] {
        let mut cfg = FakeConfig::new(48_000, 48_000);
        cfg.blocks_out = varied(48_000);
        cfg.stamp_jitter_out_s = 0.0;
        cfg.path.gain = 1.0;
        cfg.path.latency_s = 0.0873;
        cfg.notify_out_xrun = notify;
        let origin = cfg.host_origin_ns as f64;
        let rig = Rig::open_with(cfg, 3.0, |t| t.dc = 0.5);
        rig.handle.pump(1.0);
        let blocks_before = rig.tape().out_stamps.len();
        assert!(rig.handle.inject_xrun(Side::Out, 480));
        rig.handle.pump(1.0);
        rig.assert_path_sound();
        let truth = rig.truth();
        let tape = rig.tape();
        if notify {
            assert_eq!(tape.events, [BackendEvent::XrunOut]);
        } else {
            assert!(
                tape.events.is_empty(),
                "WASAPI does not report output underruns"
            );
        }
        assert_eq!(rig.handle.stats().xruns_out, 1);
        assert!(
            tape.out_stamps
                .windows(2)
                .all(|w| w[1].first == w[0].first + u64::from(w[0].frames))
        );
        let first_after = tape.out_stamps[blocks_before].first;
        for (k, s) in tape.out_stamps.iter().enumerate() {
            let lost = if k >= blocks_before { 480.0 } else { 0.0 };
            let t = (s.device_ns as f64 - origin) * 1.0e-9;
            let expect = truth.out_time_s(0.0) + (s.first as f64 + lost) / 48_000.0;
            assert!((t - expect).abs() < 2.0e-9, "stamp {k}");
        }
        // The microphone hears 10 ms of silence, ending where the first frame after
        // the gap arrives.
        let below: Vec<usize> = (10_000..tape.rec.len())
            .filter(|g| tape.rec[*g] < 0.25)
            .collect();
        let (start, end) = (below[0] as f64, *below.last().unwrap() as f64 + 1.0);
        assert_eq!(below.len(), (end - start) as usize, "one contiguous dip");
        assert!(
            (end - start - 480.0).abs() <= 1.0,
            "dip of {} frames",
            end - start
        );
        let arrival = truth.arrival_in(first_after as f64);
        assert!(
            (end - arrival).abs() <= 1.0,
            "dip ends at {end}, expected {arrival}"
        );
        assert!((tape.rec[start as usize + 240]).abs() < 1.0e-3);
        assert!((tape.rec.last().unwrap() - 0.5).abs() < 1.0e-3);
    }
}

// ---------------------------------------------------------------- device change

#[test]
fn device_change_stops_the_streams_and_reopen_starts_over() {
    let rig = Rig::open(FakeConfig::default(), 2.0);
    assert!(rig.handle.is_open() && rig.handle.is_running());
    rig.handle.pump(0.3);
    // A default-device change on CoreAudio: reported, streams keep running.
    rig.handle.inject_event(BackendEvent::DeviceChanged);
    assert!(rig.handle.pump(0.3) > 0);
    assert_eq!(rig.tape().events, [BackendEvent::DeviceChanged]);
    assert!(rig.handle.is_running());
    // The stream is invalidated: reported once, then nothing runs.
    let before = rig.handle.stats();
    rig.handle.inject_event(BackendEvent::Invalidated);
    rig.handle.inject_event(BackendEvent::DeviceLost);
    assert_eq!(rig.handle.pump(0.5), 0);
    assert_eq!(
        rig.tape().events,
        [BackendEvent::DeviceChanged, BackendEvent::Invalidated],
        "nothing is reported after the stream died"
    );
    let after = rig.handle.stats();
    assert_eq!(after.callbacks_in, before.callbacks_in);
    assert_eq!(after.callbacks_out, before.callbacks_out);
    assert_eq!(after.events, 2);
    assert!(rig.handle.is_open() && !rig.handle.is_running());
    assert_eq!(rig.handle.pump_next(), None);
    assert!(!rig.handle.inject_xrun(Side::In, 10));
    assert!(
        (rig.handle.now_s() - 1.1).abs() < 1.0e-9,
        "time still passes"
    );

    // Dropping the streams drops the callbacks (and their hold on the tape).
    let Rig {
        backend,
        handle,
        tape,
        streams,
        ..
    } = rig;
    assert!(
        backend
            .open(&OpenRequest::default(), callbacks(&tape))
            .is_err()
    );
    assert_eq!(Arc::strong_count(&tape), 4);
    drop(streams);
    assert_eq!(Arc::strong_count(&tape), 1);
    assert!(!handle.is_open());
    assert!(handle.truth().is_none() && handle.next_frames().is_none());

    // Reopen: a new pair of streams, counters from zero, at the current time.
    let old_blocks = tape.lock().unwrap().in_stamps.len();
    let (streams, _) = backend
        .open(&OpenRequest::default(), callbacks(&tape))
        .unwrap();
    assert!(handle.pump(0.3) > 0);
    {
        let t = tape.lock().unwrap();
        assert_eq!(t.in_stamps[old_blocks].first, 0);
        assert!(t.in_stamps[old_blocks].device_ns > t.in_stamps[old_blocks - 1].device_ns);
        assert_eq!(t.events.len(), 2, "the queued DeviceLost was discarded");
    }
    assert_eq!(handle.stats().opens, 2);
    let (next_in, next_out) = handle.next_frames().unwrap();
    assert!(
        next_in > 13_000 && next_in % 480 == 0 && next_out > next_in,
        "{next_in} {next_out}"
    );
    drop(streams);
}

// ---------------------------------------------------------------- external sound

#[test]
fn external_sound_lands_on_the_returned_frame() {
    let mut cfg = FakeConfig::new(44_100, 48_000);
    cfg.drift_in_ppm = -150.0;
    cfg.blocks_in = varied(44_100);
    cfg.path.gain = 0.0;
    let rig = Rig::open(cfg, 3.0);
    let pluck: Arc<[f32]> = Arc::from(vec![1.0f32, 0.5, 0.25, -0.5]);
    rig.handle.pump(0.3);
    let check = |at_s: f64| {
        let frame = rig
            .handle
            .play_external(at_s, Arc::clone(&pluck), 0.5)
            .expect("accepted");
        let truth = rig.truth();
        let off = (truth.in_time_s(frame as f64) - at_s) * truth.rate_in();
        assert!(
            off.abs() <= 0.5 + 1.0e-6,
            "rounded to the nearest frame: {off}"
        );
        frame as usize
    };
    let a = check(0.5);
    let b = check(0.5001); // right behind the first
    rig.handle.pump(0.5);
    rig.handle.inject_xrun(Side::In, 3000);
    let c = check(1.2);
    assert!(
        rig.handle
            .play_external(-1.0, Arc::clone(&pluck), 1.0)
            .is_none()
    );
    rig.handle.pump(0.7);
    let tape = rig.tape();
    assert_eq!(b - a, 4, "0.1 ms is 4.41 frames");
    assert_eq!(tape.rec[a..a + 4], [0.5, 0.25, 0.125, -0.25]);
    assert_eq!(tape.rec[b..b + 4], [0.5, 0.25, 0.125, -0.25]);
    assert_eq!(
        tape.rec[c..c + 4],
        [0.5, 0.25, 0.125, -0.25],
        "after an xrun too"
    );
    let heard = tape.rec.iter().filter(|v| **v != 0.0).count();
    assert_eq!(heard, 12);
}

// ---------------------------------------------------------------- determinism, allocation

fn busy_config(seed: u32) -> FakeConfig {
    let mut cfg = path_config(PATHS[5], seed);
    cfg.path.noise_rms = 0.002;
    let mut ir = vec![0.0f32; 200];
    ir[0] = 1.0;
    ir[130] = 0.3;
    cfg.path.ir = Some(ir);
    cfg
}

fn busy_rig(seed: u32, seconds: f64) -> Rig {
    let chirp: Arc<[f32]> = Arc::from(Probe::new(48_000.0).signal());
    let rig = Rig::open_with(busy_config(seed), seconds, |t| {
        for k in 0..40 {
            t.plays.push(Play {
                at: 4_800 + 9_111 * k,
                samples: Arc::clone(&chirp),
                gain: 0.25,
            });
        }
        t.clock = Some(ClockMap::new(44_100.0, 48_000.0, ClockConfig::default()));
    });
    rig.handle.set_external_fn(Some(Box::new(|pos, block| {
        for (i, v) in block.iter_mut().enumerate() {
            *v += 0.01 * ((pos + i as u64) % 7) as f32;
        }
    })));
    rig
}

#[test]
fn the_same_seed_repeats_for_any_slicing_of_time() {
    let whole = busy_rig(5, 3.0);
    whole.handle.pump(2.0);
    let sliced = busy_rig(5, 3.0);
    let mut rng = Mulberry32::new(1);
    let mut left = 2.0;
    while left > 0.0 {
        let dt = (0.0004 + 0.02 * rng.next_f64()).min(left);
        sliced.handle.pump(dt);
        left -= dt;
    }
    let one_by_one = busy_rig(5, 3.0);
    while one_by_one.handle.now_s() < 2.0 {
        one_by_one.handle.pump_next().unwrap();
    }
    let other = busy_rig(6, 3.0);
    other.handle.pump(2.0);

    let (a, b, c, d) = (whole.tape(), sliced.tape(), one_by_one.tape(), other.tape());
    // The slicings can differ by the callbacks that fall on the last instant.
    for t in [&b, &c] {
        let n_in = a.in_stamps.len().min(t.in_stamps.len());
        let n_out = a.out_stamps.len().min(t.out_stamps.len());
        assert!(a.in_stamps.len().abs_diff(t.in_stamps.len()) <= 1);
        assert!(a.out_stamps.len().abs_diff(t.out_stamps.len()) <= 1);
        assert_eq!(a.in_stamps[..n_in], t.in_stamps[..n_in]);
        assert_eq!(a.out_stamps[..n_out], t.out_stamps[..n_out]);
        let n = a.rec.len().min(t.rec.len());
        assert!(n > 80_000);
        assert!(a.rec[..n] == t.rec[..n], "samples differ");
    }
    assert!(a.rec.iter().any(|v| v.abs() > 0.05), "the probes are heard");
    assert_ne!(
        a.in_stamps[..100],
        d.in_stamps[..100],
        "another seed, other stamps"
    );
}

#[test]
fn pump_does_not_allocate() {
    let rig = busy_rig(9, 8.0);
    let pluck: Arc<[f32]> = Arc::from(vec![0.2f32; 2000]);
    rig.handle.pump(0.5);
    for k in 0..20 {
        rig.handle
            .play_external(0.6 + 0.2 * f64::from(k), Arc::clone(&pluck), 0.5)
            .unwrap();
    }
    let ran = assert_no_alloc(|| {
        let mut ran = 0;
        for k in 0..60 {
            ran += rig.handle.pump(0.1);
            match k % 10 {
                2 => {
                    rig.handle.inject_xrun(Side::In, 441);
                }
                5 => {
                    rig.handle.inject_xrun(Side::Out, 480);
                }
                7 => rig.handle.inject_stamp_outlier(Side::Out, 0.02),
                8 => rig.handle.inject_event(BackendEvent::DeviceChanged),
                _ => {}
            }
            ran += usize::from(rig.handle.pump_next().is_some());
        }
        rig.handle.inject_event(BackendEvent::Invalidated);
        rig.handle.pump(0.1);
        ran
    });
    assert!(ran > 1000);
    let st = rig.handle.stats();
    assert_eq!((st.xruns_in, st.xruns_out), (6, 6));
    assert_eq!(
        (st.path_starved, st.path_overrun, st.nonfinite_out),
        (0, 0, 0)
    );
    let tape = rig.tape();
    assert_eq!(tape.events.len(), 6 + 6 + 6 + 1);
    assert_eq!(tape.events.last(), Some(&BackendEvent::Invalidated));
    assert!(!tape.steps.is_empty(), "the clock map saw the xruns");
}

#[test]
fn realtime_pump_thread_runs_and_stops() {
    let rig = Rig::open(FakeConfig::default(), 30.0);
    let guard = rig.handle.spawn_realtime(20.0);
    let start = std::time::Instant::now();
    while rig.handle.stats().callbacks_in < 50 && start.elapsed().as_secs() < 10 {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    drop(guard);
    let st = rig.handle.stats();
    assert!(st.callbacks_in >= 50, "{st:?}");
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(
        rig.handle.stats(),
        st,
        "nothing runs after the guard is dropped"
    );
    assert!(rig.handle.now_ns() > 3_600_000_000_000);
    drop(rig.streams);
}

// ---------------------------------------------------------------- ClockMap on the fake

/// Errors of the map against the truth, in ms, for the marks of `seconds`.
fn map_errors(tape: &Tape, truth: &FakeTruth, from_s: f64, to_s: f64) -> Vec<f64> {
    tape.marks
        .iter()
        .filter(|m| {
            let t = m.frame as f64 / truth.fs_out;
            t >= from_s && t < to_s
        })
        .map(|m| (m.in_of_out - truth.in_of_out(m.frame as f64)) / truth.fs_in * 1.0e3)
        .collect()
}

#[test]
fn clock_map_recovers_the_drift_of_the_fake() {
    // The T4 tolerances, with the stamps of the fake instead of the synthetic ones.
    // Origin offset: reported only (the default, as from the cpal backend: the map must
    // not add it) or left to the client (raw input stamps: the map adds it).
    let cases = [
        (48_000, 48_000, 200.0, 0.0, 0, false),
        (44_100, 48_000, 120.0, -80.0, 7_000_000_000i64, false),
        (44_100, 48_000, -200.0, 0.0, -250_000_000, true),
        (48_000, 44_100, 0.0, 200.0, 40_000_000, true),
    ];
    for (k, (fs_in, fs_out, d_in, d_out, origin, raw)) in cases.into_iter().enumerate() {
        let mut cfg = path_config((fs_in, fs_out, d_in, d_out, 35.0), 90 + k as u32);
        cfg.origin_offset_ns = origin;
        cfg.raw_input_stamps = raw;
        let mut rig = Rig::open(cfg, 23.0);
        let clock = ClockMap::new(f64::from(fs_in), f64::from(fs_out), rig.clock_config());
        rig.tape().clock = Some(clock);
        rig.handle.pump(22.0);
        let truth = rig.truth();
        let tape = rig.tape();
        let p = p95(&map_errors(&tape, &truth, 5.0, 20.0));
        let drift_err = tape
            .marks
            .iter()
            .filter(|m| m.frame as f64 / truth.fs_out >= 20.0)
            .fold(0.0f64, |m, x| {
                m.max((x.drift_ppm - truth.drift_ppm()).abs())
            });
        println!(
            "ClockMap on the fake, {fs_in}/{fs_out} Hz, drift {d_in:+.0}/{d_out:+.0} ppm: p95 error 5..20 s {p:.4} ms (gate 0.10), drift error 20..22 s {drift_err:.2} ppm (gate 10)"
        );
        assert!(p <= 0.10, "p95 {p} ms");
        assert!(drift_err <= 10.0, "drift error {drift_err} ppm");
        assert_eq!(tape.marks.last().unwrap().quality, ClockQuality::Locked);
        assert!(tape.steps.is_empty(), "{:?}", tape.steps);
        drop(tape);
        rig.streams = None;
    }
}

#[test]
fn clock_map_finds_an_xrun_of_the_fake_within_3_callbacks() {
    // 20 ms lost on each side in turn. The output underrun is not reported (WASAPI);
    // the input overrun is, but the notice is not forwarded: stamps alone must do.
    for (side, forward) in [(Side::In, false), (Side::Out, false), (Side::In, true)] {
        let mut cfg = path_config(PATHS[4], 120);
        cfg.notify_out_xrun = false;
        let rig = Rig::open(cfg, 20.0);
        let clock = ClockMap::new(44_100.0, 48_000.0, rig.clock_config());
        rig.tape().clock = Some(clock);
        rig.tape().forward_xruns = forward;
        rig.handle.pump(12.0);
        let (fs, lost) = if side == Side::In {
            (44_100.0, 882)
        } else {
            (48_000.0, 960)
        };
        let fed = {
            let t = rig.tape();
            assert_eq!(t.marks.last().unwrap().quality, ClockQuality::Locked);
            if side == Side::In {
                t.in_stamps.len()
            } else {
                t.out_stamps.len()
            }
        };
        rig.handle.inject_xrun(side, lost);
        rig.handle.pump(6.0);
        let truth = rig.truth();
        let tape = rig.tape();
        assert_eq!(tape.steps.len(), 1, "{side:?}: {:?}", tape.steps);
        let (step, at) = tape.steps[0];
        let callbacks = at - fed;
        assert_eq!(step.side, side);
        assert_eq!(callbacks, if forward { 1 } else { 3 }, "{side:?}");
        let cause = if forward {
            StepCause::Notice
        } else {
            StepCause::Detected
        };
        assert_eq!(step.cause, cause);
        let rate = if side == Side::In {
            truth.rate_in()
        } else {
            truth.rate_out()
        };
        let size_err_ms = (step.step_s - f64::from(lost) / rate) * 1.0e3;
        assert!(size_err_ms.abs() <= 0.2, "step off by {size_err_ms} ms");
        assert!((step.frames - step.step_s * fs).abs() < 1.0e-9);
        // The truth knows where the frames were lost, so it holds before and after.
        let before = p95(&map_errors(&tape, &truth, 5.0, 11.9));
        let after = map_errors(&tape, &truth, 13.0, 18.0)
            .iter()
            .fold(0.0f64, |m, e| m.max(e.abs()));
        println!(
            "fake xrun {side:?} (notice forwarded: {forward}): step on callback {callbacks}, size error {size_err_ms:+.4} ms, max error 1..6 s later {after:.4} ms (gate 0.2)"
        );
        assert!(before <= 0.10, "before the xrun: {before} ms");
        assert!(after <= 0.2, "after the xrun: {after} ms");
        assert_eq!(tape.marks.last().unwrap().quality, ClockQuality::Locked);
    }
}

#[test]
fn clock_map_falls_back_when_the_fake_stamps_are_bad() {
    // Input device stamps running 0.4% fast, callback stamps sound.
    let mut cfg = path_config(PATHS[4], 130);
    cfg.stamp_skew_in_ppm = 4000.0;
    let rig = Rig::open(cfg, 16.0);
    let clock = ClockMap::new(44_100.0, 48_000.0, rig.clock_config());
    rig.tape().clock = Some(clock);
    rig.handle.pump(15.0);
    let truth = rig.truth();
    let tape = rig.tape();
    assert_eq!(tape.steps.len(), 1, "{:?}", tape.steps);
    assert_eq!(tape.steps[0].0.cause, StepCause::Fallback);
    assert_eq!(tape.steps[0].0.side, Side::In);
    let last = tape.marks.last().unwrap();
    assert_eq!(last.quality, ClockQuality::Locked);
    // Callback stamps are noisier than device stamps: 20 ppm here, not the 10 of T4.
    assert!(
        (last.drift_ppm - truth.drift_ppm()).abs() <= 20.0,
        "{}",
        last.drift_ppm
    );
    // The map sits a constant delay away from the truth (delivery lag of the input
    // callbacks, a little over in_lead_s = 2 ms): steady is what counts.
    let e = map_errors(&tape, &truth, 8.0, 15.0);
    let mean = e.iter().sum::<f64>() / e.len() as f64;
    let spread = e.iter().fold(0.0f64, |m, x| m.max((x - mean).abs()));
    assert!((-2.4..=-2.0).contains(&mean), "offset {mean} ms");
    assert!(spread <= 0.15, "spread {spread} ms");
}

// ---------------------------------------------------------------- review findings

#[test]
fn an_all_zero_impulse_response_is_silence() {
    // A response of zeros is not "no response": the microphone hears nothing.
    let mut cfg = FakeConfig::new(48_000, 48_000);
    cfg.path.gain = 1.0;
    cfg.path.ir = Some(vec![0.0; 67]);
    let rig = Rig::open_with(cfg.clone(), 1.0, |t| t.dc = 0.5);
    rig.handle.pump(0.5);
    assert!(rig.tape().rec.len() >= 20_000);
    assert!(
        rig.tape().rec.iter().all(|v| *v == 0.0),
        "heard {} through an all-zero response",
        rig.tape().rec.last().unwrap()
    );
    drop(rig);

    // A unit response is the direct path, sample for sample.
    let hear = |ir: Option<Vec<f32>>| {
        let mut cfg = cfg.clone();
        cfg.path.ir = ir;
        cfg.path.noise_rms = 0.001;
        let rig = Rig::open_with(cfg, 1.0, |t| t.dc = 0.5);
        rig.handle.pump(0.5);
        std::mem::take(&mut rig.tape().rec)
    };
    let (direct, unit) = (hear(None), hear(Some(vec![1.0])));
    assert!((direct.last().unwrap() - 0.5).abs() < 0.01);
    assert!(direct == unit);
}

#[test]
fn a_fatal_event_is_not_lost_when_the_queue_is_full() {
    let rig = Rig::open(FakeConfig::default(), 2.0);
    rig.handle.pump(0.1);
    for _ in 0..40 {
        rig.handle.inject_event(BackendEvent::Other);
    }
    rig.handle.inject_event(BackendEvent::Invalidated);
    rig.handle.inject_event(BackendEvent::DeviceLost);
    assert_eq!(rig.handle.pump(0.3), 0, "the streams ran on");
    let events = rig.tape().events.clone();
    let (fatal, rest) = events.split_last().expect("events");
    assert_eq!(*fatal, BackendEvent::Invalidated, "{events:?}");
    assert!(rest.len() == 16 && rest.iter().all(|e| *e == BackendEvent::Other));
    assert_eq!(rig.handle.stats().events_dropped, 24);
    assert_eq!(rig.handle.stats().events, 17);
    assert!(rig.handle.is_open() && !rig.handle.is_running());
}

#[test]
fn infinite_output_samples_are_counted_and_played_as_silence() {
    let mut cfg = FakeConfig::new(48_000, 48_000);
    cfg.path.gain = 1.0;
    for (clip, level) in [
        (true, f32::INFINITY),
        (true, f32::NEG_INFINITY),
        (false, f32::INFINITY),
    ] {
        cfg.clip = clip;
        let rig = Rig::open_with(cfg.clone(), 1.0, |t| t.dc = level);
        rig.handle.pump(0.5);
        let st = rig.handle.stats();
        assert!(st.frames_out >= 24_000);
        assert_eq!(
            st.nonfinite_out, st.frames_out,
            "clip {clip}, level {level}"
        );
        assert!(
            rig.tape().rec.iter().all(|v| *v == 0.0),
            "clip {clip}: {level} was played as {}",
            rig.tape().rec.last().unwrap()
        );
    }
    // One bad channel spoils the frame, with the other channel in range.
    cfg.clip = true;
    let rig = Rig::open_with(cfg, 1.0, |t| {
        t.dc = 0.25;
        t.left_only = true;
        t.plays.push(Play {
            at: 0,
            samples: Arc::from(vec![f32::INFINITY; 4_800]),
            gain: 1.0,
        });
    });
    rig.handle.pump(0.5);
    assert_eq!(rig.handle.stats().nonfinite_out, 4_800);
    let tape = rig.tape();
    // Silence, then the 0.25 of the good frames. The step rings (band-limited path), but
    // nothing comes near the 0.625 that a clipped ∞ and 0.25 would average to.
    let peak = tape.rec.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak < 0.30, "peak {peak}: the infinite samples were played");
    assert!((tape.rec.last().unwrap() - 0.25).abs() < 1.0e-3);
    println!("one infinite channel for 4800 frames: counted 4800, peak heard {peak:.4}");
}

// ---------------------------------------------------------------- xrun boundary, late notices

#[test]
fn xrun_notices_carry_the_boundary_and_can_be_late() {
    // 10 ms blocks. `late`: blocks of the stream that run before the notice is delivered.
    let cases = [
        (0.0, true, 0usize),
        (0.005, true, 1),
        (0.025, true, 3),
        (0.005, false, 1),
        (0.0, false, 0),
    ];
    for side in [Side::In, Side::Out] {
        for (delay_s, with_boundary, late) in cases {
            let cfg = FakeConfig {
                error_delay_s: delay_s,
                error_boundary: with_boundary,
                ..FakeConfig::default()
            };
            let rig = Rig::open(cfg, 3.0);
            rig.handle.pump(1.0);
            let pick = |(i, o): (u64, u64)| if side == Side::In { i } else { o };
            let boundary = pick(rig.handle.next_frames().unwrap());
            let before = {
                let t = rig.tape();
                (t.in_stamps.len(), t.out_stamps.len())
            };
            assert!(rig.handle.inject_xrun(side, 480));
            rig.handle.pump(1.0);

            let t = rig.tape();
            let what = format!("{side:?}, delay {delay_s}, boundary {with_boundary}");
            assert_eq!(t.notices.len(), 1, "{what}");
            let n = t.notices[0];
            let ev = match side {
                Side::In => BackendEvent::XrunIn,
                Side::Out => BackendEvent::XrunOut,
            };
            assert_eq!(n.ev, ev, "{what}");
            // The boundary is the frame counter at the xrun: the first frame of the block
            // that follows it, whenever the notice is delivered.
            assert_eq!(n.at, with_boundary.then_some(boundary), "{what}");
            let (stamps, ran, ran_before) = match side {
                Side::In => (&t.in_stamps, n.blocks.0, before.0),
                Side::Out => (&t.out_stamps, n.blocks.1, before.1),
            };
            assert_eq!(stamps[ran_before].first, boundary, "{what}");
            assert_eq!(ran - ran_before, late, "{what}");
            assert_eq!(rig.handle.stats().events, 1);
        }
    }
}

#[test]
fn device_events_are_not_delayed_and_have_no_boundary() {
    let cfg = FakeConfig {
        error_delay_s: 0.045,
        ..FakeConfig::default()
    };
    let rig = Rig::open(cfg, 2.0);
    rig.handle.pump(0.5);
    let before = {
        let t = rig.tape();
        (t.in_stamps.len(), t.out_stamps.len())
    };
    // An xrun whose notice is due 45 ms after its block, then a device event.
    assert!(rig.handle.inject_xrun(Side::In, 480));
    rig.handle.pump(0.020);
    assert!(rig.tape().notices.is_empty(), "the xrun notice is not due");
    rig.handle.inject_event(BackendEvent::DeviceChanged);
    let ran = {
        let t = rig.tape();
        (t.in_stamps.len(), t.out_stamps.len())
    };
    assert!(ran.0 > before.0);
    rig.handle.pump_next().unwrap();
    {
        let t = rig.tape();
        assert_eq!(
            t.notices[..],
            [Notice {
                ev: BackendEvent::DeviceChanged,
                at: None,
                blocks: ran,
            }],
            "before the next callback"
        );
    }
    rig.handle.pump(0.1);
    {
        let t = rig.tape();
        assert_eq!(t.notices.len(), 2);
        let n = t.notices[1];
        assert_eq!(n.ev, BackendEvent::XrunIn);
        assert_eq!(n.at, Some(t.in_stamps[before.0].first));
        // Due 45 ms after its block: that one and four more 10 ms blocks ran.
        assert_eq!(n.blocks.0 - before.0, 5);
    }
    // A notice that is still waiting when the device goes away is never delivered: the
    // streams are dead.
    assert!(rig.handle.inject_xrun(Side::Out, 480));
    rig.handle.pump(0.020);
    rig.handle.inject_event(BackendEvent::DeviceLost);
    rig.handle.pump(0.5);
    let t = rig.tape();
    assert_eq!(t.notices.len(), 3);
    assert_eq!(
        (t.notices[2].ev, t.notices[2].at),
        (BackendEvent::DeviceLost, None)
    );
}

#[test]
fn late_notices_do_not_depend_on_the_slicing_of_time() {
    let open = || {
        let mut cfg = busy_config(5);
        cfg.error_delay_s = 0.007;
        Rig::open(cfg, 4.0)
    };
    let run = |rig: &Rig, slice: &mut dyn FnMut() -> f64| {
        let mut next_xrun = 0.5;
        let mut k = 0u32;
        while rig.handle.now_s() < 3.0 {
            let now = rig.handle.now_s();
            if now >= next_xrun {
                let side = if k.is_multiple_of(2) {
                    Side::In
                } else {
                    Side::Out
                };
                assert!(rig.handle.inject_xrun(side, 100 + 37 * k));
                k += 1;
                next_xrun += 0.11;
            }
            // Never across the instant of the next xrun: both runs inject at the same time.
            let dt = slice().min(next_xrun - now).min(3.0 - now);
            rig.handle.pump(dt.max(1.0e-9));
        }
        k
    };
    let (whole, sliced) = (open(), open());
    let n = run(&whole, &mut || 1.0);
    let mut rng = Mulberry32::new(3);
    let m = run(&sliced, &mut || 0.0002 + 0.004 * rng.next_f64());
    assert_eq!(n, m);
    assert!(n >= 20, "{n} xruns");
    let (a, b) = (whole.tape(), sliced.tape());
    assert_eq!(a.notices.len(), n as usize);
    assert_eq!(a.notices, b.notices);
    assert!(a.notices.iter().all(|x| x.at.is_some()));
    // Late for real: the block at the boundary had run.
    for x in &a.notices {
        let (stamps, ran) = match x.ev {
            BackendEvent::XrunIn => (&a.in_stamps, x.blocks.0),
            _ => (&a.out_stamps, x.blocks.1),
        };
        let at = x.at.unwrap();
        assert!(stamps[..ran].iter().any(|s| s.first == at), "{x:?}");
    }
    let n_in = a.in_stamps.len().min(b.in_stamps.len());
    assert_eq!(a.in_stamps[..n_in], b.in_stamps[..n_in]);
}

#[test]
fn the_streams_read_the_host_clock_of_the_fake() {
    let cfg = FakeConfig {
        host_origin_ns: 77_000_000_123,
        ..FakeConfig::default()
    };
    let rig = Rig::open(cfg, 1.0);
    let streams = rig.streams.as_ref().unwrap();
    assert_eq!(streams.host_now_ns(), 77_000_000_123);
    rig.handle.pump(0.25);
    assert_eq!(streams.host_now_ns(), 77_250_000_123);
    assert_eq!(streams.host_now_ns(), rig.handle.now_ns());
    // The stamps are on that clock: no block was requested in the future.
    let now = streams.host_now_ns();
    {
        let t = rig.tape();
        assert!(
            t.out_stamps
                .iter()
                .chain(&t.in_stamps)
                .all(|s| s.callback_ns <= now)
        );
        assert!(
            t.out_stamps.last().unwrap().device_ns > now,
            "the output plays ahead"
        );
    }
    // It keeps running when no callback does.
    rig.handle.inject_event(BackendEvent::DeviceLost);
    rig.handle.pump(0.5);
    assert_eq!(streams.host_now_ns(), 77_750_000_123);
}

#[test]
fn a_negative_error_delay_is_refused() {
    let tape = Rig::open(FakeConfig::default(), 0.1).tape.clone();
    for delay in [-0.001, f64::NAN, f64::INFINITY] {
        let b = FakeBackend::new(FakeConfig {
            error_delay_s: delay,
            ..FakeConfig::default()
        });
        assert!(
            matches!(
                b.open(&OpenRequest::default(), callbacks(&tape)),
                Err(AudioError::Backend(_))
            ),
            "{delay}"
        );
    }
}
