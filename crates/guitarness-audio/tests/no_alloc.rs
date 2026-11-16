//! T7: no allocation inside the engine's callbacks. 10 virtual minutes on the fake
//! backend with xruns, mixer commands, strums at the microphone and backend events, every
//! callback body wrapped in `assert_no_alloc` (the process aborts on an allocation).
//! The callbacks also publish the playhead stamp and fill the raw ring of the tape
//! (Pulso, units A0 and A1). Debug build. The negative control proves the checker is live.
//!
//! Only the callbacks are checked: the analysis step and the fake's own pump may allocate.

use std::sync::atomic::{AtomicU64, Ordering};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use guitarness_audio::{
    AudioEngine, AudioError, Backend, BackendEvent, DeviceInfo, EngineConfig, EngineEvent,
    FakeBackend, FakeConfig, IoCallbacks, MixCmd, OpenRequest, Pump, Side, SoundBank, SoundId,
    StreamInfo,
};

#[global_allocator]
static A: AllocDisabler = AllocDisabler;

static GUARDED_IN: AtomicU64 = AtomicU64::new(0);
static GUARDED_OUT: AtomicU64 = AtomicU64::new(0);
static GUARDED_ERR: AtomicU64 = AtomicU64::new(0);

/// Wraps every callback of the inner backend in `assert_no_alloc`.
struct Guard<B: Backend>(B);

impl<B: Backend> Backend for Guard<B> {
    type Streams = B::Streams;

    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        self.0.devices()
    }

    fn open(
        &self,
        req: &OpenRequest,
        io: IoCallbacks,
    ) -> Result<(Self::Streams, StreamInfo), AudioError> {
        let IoCallbacks {
            mut input,
            mut output,
            mut error,
        } = io;
        let io = IoCallbacks {
            input: Box::new(move |b, s| {
                GUARDED_IN.fetch_add(1, Ordering::Relaxed);
                assert_no_alloc(|| input(b, s));
            }),
            output: Box::new(move |b, s| {
                GUARDED_OUT.fetch_add(1, Ordering::Relaxed);
                assert_no_alloc(|| output(b, s));
            }),
            error: Box::new(move |e, at| {
                GUARDED_ERR.fetch_add(1, Ordering::Relaxed);
                assert_no_alloc(|| error(e, at));
            }),
        };
        self.0.open(req, io)
    }
}

/// A plucked string every 0.4 s at the microphone: decaying 220 Hz with a hard attack.
fn pluck_source() -> guitarness_audio::ExternalFn {
    Box::new(|pos: u64, buf: &mut [f32]| {
        const PERIOD: u64 = 19_200;
        for (i, v) in buf.iter_mut().enumerate() {
            let t = pos + i as u64;
            let ph = (t % PERIOD) as f32;
            let env = (-ph / 3000.0).exp();
            let w = std::f32::consts::TAU * 220.0 / 48_000.0;
            *v += 0.3 * env * ((ph * w).sin() + 0.5 * (2.0 * ph * w).sin());
        }
    })
}

#[test]
fn t7_ten_virtual_minutes_without_allocation() {
    let fc = FakeConfig {
        drift_in_ppm: 40.0,
        ..FakeConfig::default()
    };
    let backend = FakeBackend::new(fc);
    let h = backend.handle();
    let (mut eng, mut io) = AudioEngine::start(
        Guard(backend),
        EngineConfig::default(),
        SoundBank::new(48_000.0),
        Pump::Manual,
    )
    .unwrap();
    h.set_external_fn(Some(pluck_source()));
    io.tap.set(true);
    io.tap.set_raw(true);
    let mut raw = io.tap.take_raw().expect("the raw ring");
    let ph = eng.playhead();
    let mut raw_buf: Vec<f32> = Vec::with_capacity(2048);
    let (mut raw_frames, mut raw_next) = (0u64, 0u64);
    let mut last_mark = None;

    let mut out_frame = 0u64;
    let mut sent = 0u64;
    let mut refused = 0u64;
    let mut events = 0u64;
    let mut tap_buf = vec![0.0f32; 96_000];
    let mut tap_total = 0usize;
    let mut steps = 0u64;
    let secs = 600u32;
    let slice = 0.05;
    let n = (f64::from(secs) / slice) as u32;
    for k in 0..n {
        let t = f64::from(k) * slice;
        // Mixer commands: clicks, one-shots, a metronome that is stopped and restarted.
        if k % 7 == 0 {
            let ok = io.mix.send(MixCmd::Play {
                tag: k,
                sound: [SoundId::Click, SoundId::Accent, SoundId::Probe][(k % 3) as usize],
                at: out_frame + 2400 + u64::from(k % 5) * 100,
                gain: 0.3,
            });
            sent += u64::from(ok);
            refused += u64::from(!ok);
        }
        if k % 400 == 1 {
            let _ = io.mix.send(MixCmd::Metronome {
                tag: 900,
                start: out_frame + 4800,
                period: 24_000.7,
                accent_every: 4,
                beats: 0,
                gain: 0.4,
            });
        }
        if k % 400 == 300 {
            let _ = io.mix.send(MixCmd::StopTag {
                tag: 900,
                at: out_frame + 4800,
            });
        }
        if k % 997 == 5 {
            let _ = io.mix.send(MixCmd::Silence);
        }
        // Xruns on both sides, backend events that do not stop the streams.
        if k % 211 == 17 {
            h.inject_xrun(Side::In, 960 + (k % 4) * 240);
        }
        if k % 263 == 40 {
            h.inject_xrun(Side::Out, 1200 + (k % 3) * 480);
        }
        if k % 349 == 9 {
            h.inject_event(BackendEvent::Other);
        }
        if k % 1000 == 500 {
            h.inject_event(BackendEvent::RealtimeDenied);
        }
        h.pump(slice);
        eng.pump();
        out_frame = (t * 48_000.0) as u64 + 2400;
        while let Ok(e) = io.events.pop() {
            events += 1;
            steps += u64::from(matches!(e, EngineEvent::Step(_)));
        }
        if io.tap.available() > 48_000 {
            tap_total += io.tap.read(&mut tap_buf);
        }
        // The tape side: whole blocks, in order, none missing.
        while let Some(m) = raw.read_block(&mut raw_buf) {
            assert_eq!(m.in_first, raw_next, "a raw block is missing");
            raw_next = m.in_first + u64::from(m.frames);
            raw_frames += u64::from(m.frames);
        }
        // The drawing side: reading the playhead does not allocate either.
        let mark = assert_no_alloc(|| ph.last_out()).expect("a block was played");
        assert!(last_mark.is_none_or(|l: u64| mark.first >= l));
        last_mark = Some(mark.first);
    }
    tap_total += io.tap.read(&mut tap_buf);
    let st = eng.stop();
    while let Some(m) = raw.read_block(&mut raw_buf) {
        assert_eq!(m.in_first, raw_next, "a raw block is missing");
        raw_next = m.in_first + u64::from(m.frames);
        raw_frames += u64::from(m.frames);
    }
    let mark = ph.last_out().expect("the last stamp stays");
    let (gi, go, ge) = (
        GUARDED_IN.load(Ordering::Relaxed),
        GUARDED_OUT.load(Ordering::Relaxed),
        GUARDED_ERR.load(Ordering::Relaxed),
    );
    eprintln!(
        "T7: guarded callbacks in {gi}, out {go}, error {ge}; commands sent {sent} refused \
         {refused}; events {events}, steps {steps}, tap samples {tap_total}, raw frames \
         {raw_frames}, last stamp {mark:?}\n    {st:?}"
    );
    // Not vacuous: 600 s of 10 ms blocks on each side, and the whole scenario ran.
    assert!(gi >= 55_000 && go >= 55_000, "{gi} {go}");
    assert!(ge > 50);
    assert!(st.xruns_in > 40 && st.xruns_out > 40);
    assert!(st.steps_in > 40 && st.steps_out > 40);
    assert!(st.onsets > 500, "{}", st.onsets);
    assert!(tap_total > 1_000_000);
    // Every input frame went through the raw ring, and the last stamp published is the
    // last output block.
    assert_eq!(raw.dropped_blocks(), 0);
    assert_eq!(raw_frames, st.frames_in);
    assert_eq!(mark.first + u64::from(mark.frames), st.frames_out);
    assert_eq!(st.nonfinite_out, 0);
    assert_eq!(st.skipped_in + st.skipped_out, 0);
    assert!(st.frames_out >= 600 * 48_000 - 1_000_000);
}

/// Child half of the negative control: allocates inside `assert_no_alloc`, so the
/// process must abort. Does nothing unless the parent sets the variable.
#[test]
fn t7_negative_control_child() {
    if std::env::var_os("GUITARNESS_ALLOC_CHILD").is_none() {
        return;
    }
    assert_no_alloc(|| {
        let v: Vec<u8> = std::hint::black_box(Vec::with_capacity(64));
        std::hint::black_box(&v);
    });
    // Reaching here means the check did not fire.
}

#[test]
fn t7_negative_control_aborts() {
    let exe = std::env::current_exe().unwrap();
    let st = std::process::Command::new(exe)
        .args(["--exact", "t7_negative_control_child", "--nocapture"])
        .env("GUITARNESS_ALLOC_CHILD", "1")
        .status()
        .unwrap();
    assert!(!st.success(), "allocating under assert_no_alloc must abort");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(st.signal(), Some(6), "expected SIGABRT, got {st:?}");
    }
}

/// Second negative control, through the engine path: an allocation injected *inside* a
/// callback that `Guard` wraps must abort. `Alloc` sits above `Guard` (it wraps the engine's callback before `Guard` wraps it again), so if `Guard` stopped
/// wrapping the callbacks this child would survive and the parent test would fail.
#[test]
fn t7_guard_negative_control_child() {
    if std::env::var_os("GUITARNESS_ALLOC_CHILD2").is_none() {
        return;
    }
    struct Alloc<B: Backend>(B);
    impl<B: Backend> Backend for Alloc<B> {
        type Streams = B::Streams;
        fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
            self.0.devices()
        }
        fn open(
            &self,
            req: &OpenRequest,
            io: IoCallbacks,
        ) -> Result<(Self::Streams, StreamInfo), AudioError> {
            let IoCallbacks {
                mut input,
                output,
                error,
            } = io;
            let io = IoCallbacks {
                input: Box::new(move |b, s| {
                    input(b, s);
                    let v: Vec<u8> = std::hint::black_box(Vec::with_capacity(8));
                    std::hint::black_box(&v);
                }),
                output,
                error,
            };
            self.0.open(req, io)
        }
    }
    let backend = FakeBackend::new(FakeConfig::default());
    let h = backend.handle();
    let (_eng, _io) = AudioEngine::start(
        Alloc(Guard(backend)),
        EngineConfig::default(),
        SoundBank::new(48_000.0),
        Pump::Manual,
    )
    .unwrap();
    h.pump(0.1);
}

#[test]
fn t7_guard_negative_control_aborts() {
    let exe = std::env::current_exe().unwrap();
    let st = std::process::Command::new(exe)
        .args(["--exact", "t7_guard_negative_control_child", "--nocapture"])
        .env("GUITARNESS_ALLOC_CHILD2", "1")
        .status()
        .unwrap();
    assert!(!st.success());
}
