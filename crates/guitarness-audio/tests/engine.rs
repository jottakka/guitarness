//! U6: the engine on the fake backend, with the manual pump (deterministic) and, for the
//! device-change test, the analysis thread. Run with `--nocapture` to see the measured
//! numbers.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use guitarness_audio::capture::{RawMark, RawOutput};
use guitarness_audio::{
    AudioEngine, BackendEvent, EngineConfig, EngineEvent, EngineIo, FakeBackend, FakeConfig,
    FakeHandle, MixCmd, MixEvent, Pump, Ring, Side, SoundBank, SoundId, StepCause,
};
use guitarness_dsp::{OnsetConfig, OnsetDetector, OnsetEvent};
use guitarness_sim::timing::{Cond, make_take};

type Eng = AudioEngine<FakeBackend>;

fn start_with(fc: FakeConfig, cfg: EngineConfig, pump: Pump) -> (FakeHandle, Eng, EngineIo) {
    let fs_out = f64::from(fc.fs_out);
    let backend = FakeBackend::new(fc);
    let handle = backend.handle();
    let (eng, io) = AudioEngine::start(backend, cfg, SoundBank::new(fs_out), pump).unwrap();
    (handle, eng, io)
}

fn start(fc: FakeConfig) -> (FakeHandle, Eng, EngineIo) {
    start_with(fc, EngineConfig::default(), Pump::Manual)
}

fn drain(io: &mut EngineIo, into: &mut Vec<EngineEvent>) {
    while let Ok(e) = io.events.pop() {
        into.push(e);
    }
}

/// Pumps `seconds` of virtual time in 5 ms slices, running the analysis step after each.
fn run(h: &FakeHandle, eng: &mut Eng, io: &mut EngineIo, seconds: f64, ev: &mut Vec<EngineEvent>) {
    let n = (seconds / 0.005).round() as usize;
    for _ in 0..n {
        h.pump(0.005);
        eng.pump();
        drain(io, ev);
    }
}

fn take_for(fs: f64, events: usize, seed: u32) -> guitarness_sim::timing::Take {
    let mut c = Cond::base("eng", seed);
    c.sr = fs;
    c.events = events;
    make_take(&c)
}

fn strums(ev: &[EngineEvent]) -> Vec<(f64, bool)> {
    ev.iter()
        .filter_map(|e| match e {
            EngineEvent::Onset(n) if matches!(n.event, OnsetEvent::Strum(_)) => {
                Some((n.t_in, n.judged))
            }
            _ => None,
        })
        .collect()
}

/// Runs the whole take through the fake and returns the engine's strum times and the
/// reference of the bare detector, both as input frames.
fn onset_case(fs_in: u32, fs_out: u32, drift_in: f64, drift_out: f64, metro: bool) {
    let mut fc = FakeConfig::new(fs_in, fs_out);
    fc.drift_in_ppm = drift_in;
    fc.drift_out_ppm = drift_out;
    let (h, mut eng, mut io) = start(fc);
    let take = take_for(f64::from(fs_in), 12, 11);
    let f0 = h
        .play_external(3.5, take.audio.clone().into(), 1.0)
        .expect("external sound accepted") as f64;
    // What the microphone hears (the fake path is silent without a metronome): silence up
    // to the first frame of the take, then the take. The bare detector on that signal is
    // the reference, in the same frame count as the engine.
    let mut mic = vec![0.0f32; f0 as usize];
    mic.extend_from_slice(&take.audio);
    let mut det = OnsetDetector::new(f64::from(fs_in), OnsetConfig::default());
    let mut ref_strums = Vec::new();
    for b in mic.chunks(256) {
        det.process(b, |e| {
            if let OnsetEvent::Strum(s) = e {
                ref_strums.push(s.t);
            }
        });
    }
    det.flush(|e| {
        if let OnsetEvent::Strum(s) = e {
            ref_strums.push(s.t);
        }
    });
    if metro {
        let period = f64::from(fs_out) * 60.0 / 120.0;
        assert!(io.mix.send(MixCmd::Metronome {
            tag: 7,
            start: u64::from(fs_out) / 2,
            period,
            accent_every: 4,
            beats: 0,
            gain: 0.5,
        }));
    }
    let mut ev = Vec::new();
    let secs = take.audio.len() as f64 / f64::from(fs_in) + 5.0;
    run(&h, &mut eng, &mut io, secs, &mut ev);

    let got = strums(&ev);
    assert_eq!(
        got.len(),
        ref_strums.len(),
        "strum count: engine {} vs bare detector {}",
        got.len(),
        ref_strums.len()
    );
    let mut max_map = 0.0f64;
    let mut errs = Vec::new();
    for (i, ((t_in, judged), r)) in got.iter().zip(&ref_strums).enumerate() {
        assert!(*judged, "strum {i} unexpectedly not judged");
        max_map = max_map.max((t_in - r).abs());
        // The step from digital silence to the take's noise floor at f0 is an onset of
        // the test signal, not of the guitar: no truth for it.
        if *r > f0 + 0.1 * f64::from(fs_in) {
            errs.push((t_in - (f0 + take.truth[errs.len()])) / f64::from(fs_in) * 1e3);
        }
    }
    errs.sort_by(f64::total_cmp);
    let median = errs[errs.len() / 2];
    let worst = errs.iter().fold(0.0f64, |m, e| m.max(e.abs()));
    eprintln!(
        "onset {fs_in}/{fs_out} drift {drift_in}/{drift_out} ppm metro={metro}: mapping error vs \
         bare detector {:.4} frames ({:.4} ms); vs truth median {median:.2} ms worst {worst:.2} ms",
        max_map,
        max_map / f64::from(fs_in) * 1e3,
    );
    // Without a metronome the signal is the reference's: the mapping is exact (one frame
    // is 0.02 ms). With the clicks leaking into the microphone the detector itself moves a
    // little; the 1 ms of the plan still holds.
    let limit = if metro {
        f64::from(fs_in) * 1.0e-3
    } else {
        1.0
    };
    assert!(max_map < limit, "mapping error {max_map} frames");
    // Detector bias against the exact truth (DESIGN: bias <= 2 ms).
    assert!(median.abs() < 2.0, "median detector bias {median} ms");

    // Heard comes before its Strum and maps the same way.
    let heard = ev
        .iter()
        .filter(
            |e| matches!(e, EngineEvent::Onset(n) if matches!(n.event, OnsetEvent::Heard { .. })),
        )
        .count();
    assert!(heard >= got.len());

    let clocks: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Clock(c) => Some(*c),
            _ => None,
        })
        .collect();
    let last = clocks.last().expect("clock snapshots");
    let truth = h.truth().unwrap();
    eprintln!(
        "  clock: {} snapshots, quality {:?}, drift {:.2} ppm (truth {:.2})",
        clocks.len(),
        last.quality(),
        last.drift_ppm(),
        truth.drift_ppm()
    );
    assert_eq!(last.quality(), guitarness_audio::ClockQuality::Locked);
    assert!((last.drift_ppm() - truth.drift_ppm()).abs() < 10.0);
    // About every 100 ms of output time.
    assert!(
        clocks.len() as f64 >= secs * 9.0,
        "{} snapshots",
        clocks.len()
    );

    let st = eng.stop();
    assert_eq!(
        st.drops.in_events + st.drops.events + st.drops.out_stamps,
        0
    );
    assert_eq!(st.skipped_in + st.skipped_out, 0);
    assert_eq!(st.nonfinite_out, 0);
    assert_eq!(st.steps_in + st.steps_out, 0, "no step without an xrun");
    assert!(st.callbacks_in > 100 && st.callbacks_out > 100);
    assert!(st.worst_in_ns > 0 && st.worst_out_ns > 0);
    eprintln!(
        "  worst callback: in {} us, out {} us",
        st.worst_in_ns / 1000,
        st.worst_out_ns / 1000
    );
}

#[test]
fn onset_in_frame_matches_truth_48_48() {
    onset_case(48_000, 48_000, 0.0, 0.0, false);
}

#[test]
fn onset_in_frame_matches_truth_44k1_48k_with_drift_and_metronome() {
    onset_case(44_100, 48_000, 80.0, -30.0, true);
}

#[test]
fn mixer_events_and_commands_flow() {
    let (h, mut eng, mut io) = start(FakeConfig::default());
    assert!(io.mix.send(MixCmd::Metronome {
        tag: 3,
        start: 24_000,
        period: 12_000.4,
        accent_every: 4,
        beats: 0,
        gain: 0.5,
    }));
    assert!(io.mix.send(MixCmd::Play {
        tag: 4,
        sound: SoundId::Probe,
        at: 48_000,
        gain: 0.2,
    }));
    let mut ev = Vec::new();
    run(&h, &mut eng, &mut io, 2.0, &mut ev);
    let beats: Vec<(u32, u64)> = ev
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Mix(MixEvent::Beat { tag: 3, index, at }) => Some((*index, *at)),
            _ => None,
        })
        .collect();
    assert!(beats.len() >= 6, "{beats:?}");
    for (k, at) in &beats {
        assert_eq!(*at, 24_000 + (f64::from(*k) * 12_000.4).round() as u64);
    }
    assert!(ev.contains(&EngineEvent::Mix(MixEvent::Started { tag: 4, at: 48_000 })));
    // The sound reached the microphone through the fake path; the engine is quiet.
    assert_eq!(eng.stats().drops.mix_events, 0);
}

fn windows(ev: &[EngineEvent]) -> Vec<(f64, f64)> {
    ev.iter()
        .filter_map(|e| match e {
            EngineEvent::Unjudged { from_in, to_in, .. } => Some((*from_in, *to_in)),
            _ => None,
        })
        .collect()
}

#[test]
fn xrun_makes_a_step_and_a_not_judged_window() {
    let fs = 48_000.0;
    let (h, mut eng, mut io) = start(FakeConfig::default());
    let take = take_for(fs, 24, 5);
    let f0 = h
        .play_external(3.5, take.audio.clone().into(), 1.0)
        .unwrap() as f64;
    let mut ev = Vec::new();

    // Case 1: the input xrun comes 50 ms before onset 9: that onset is inside the window
    // and is flagged as it is delivered. Case 2: it comes 20 ms after onset 15, which has
    // already been delivered as judged: only `Unjudged` can recall it.
    let cases = [(9usize, -0.050), (15, 0.020)];
    for (k, rel_s) in cases {
        let target = f0 + take.truth[k] + rel_s * fs;
        while (h.next_frames().unwrap().0 as f64) < target {
            h.pump(0.005);
            eng.pump();
            drain(&mut io, &mut ev);
        }
        assert!(h.inject_xrun(Side::In, 960));
        run(&h, &mut eng, &mut io, 0.6, &mut ev);
    }
    // An output xrun, away from every onset used above.
    let target = f0 + take.truth[20] - 0.10 * fs;
    while (h.next_frames().unwrap().0 as f64) < target {
        h.pump(0.005);
        eng.pump();
        drain(&mut io, &mut ev);
    }
    assert!(h.inject_xrun(Side::Out, 1440));
    run(&h, &mut eng, &mut io, 1.0, &mut ev);
    run(&h, &mut eng, &mut io, 3.0, &mut ev);

    let steps: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Step(s) => Some(*s),
            _ => None,
        })
        .collect();
    eprintln!("steps: {steps:?}");
    let steps_in: Vec<_> = steps.iter().filter(|s| s.side == Side::In).collect();
    let steps_out: Vec<_> = steps.iter().filter(|s| s.side == Side::Out).collect();
    assert_eq!(steps_in.len(), 2, "one step per input xrun");
    assert_eq!(steps_out.len(), 1, "one step for the output xrun");
    for s in &steps_in {
        assert!(
            (s.frames - 960.0).abs() < 48.0,
            "step of {} frames",
            s.frames
        );
    }
    assert!(steps_in.iter().all(|s| s.cause == StepCause::Notice));
    assert!(ev.contains(&EngineEvent::Backend(BackendEvent::XrunIn)));
    assert!(ev.contains(&EngineEvent::Backend(BackendEvent::XrunOut)));
    let st = eng.stats();
    assert_eq!((st.xruns_in, st.xruns_out), (2, 1));
    assert_eq!((st.steps_in, st.steps_out), (2, 1));

    // Every Step is followed by its window, 100 ms before to 250 ms after.
    let w = windows(&ev);
    assert_eq!(w.len(), 3);
    for s in &steps_in {
        let c = s.frame as f64;
        assert!(
            w.iter().any(|&(lo, hi)| (lo - (c - 0.1 * fs)).abs() < 1e-6
                && (hi - (c + 0.25 * fs)).abs() < 1e-6),
            "no window around frame {c}"
        );
    }

    // The flag is exactly "inside a window known when it was delivered", and the windows
    // recall the rest.
    let onsets: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Onset(n) => Some(*n),
            _ => None,
        })
        .collect();
    let in_any = |t: f64| w.iter().any(|&(lo, hi)| t >= lo && t <= hi);
    let flagged: Vec<_> = onsets.iter().filter(|n| !n.judged).collect();
    assert!(!flagged.is_empty(), "no onset was flagged as not judged");
    assert!(
        flagged.iter().all(|n| in_any(n.t_in)),
        "flag outside any window"
    );
    let recalled: Vec<_> = onsets
        .iter()
        .filter(|n| n.judged && in_any(n.t_in))
        .collect();
    assert!(
        !recalled.is_empty(),
        "case 2 should leave a judged onset that a later window recalls"
    );
    // Far from every glitch, everything is judged.
    for n in &onsets {
        if !in_any(n.t_in) {
            assert!(n.judged);
        }
    }
    let unj_strums = strums(&ev).iter().filter(|(_, j)| !j).count();
    eprintln!(
        "onsets {} (flagged {}, recalled by Unjudged {}), unjudged strums {unj_strums}",
        onsets.len(),
        flagged.len(),
        recalled.len()
    );
    let _ = eng.stop();
}

#[test]
fn device_invalidated_stops_cleanly_manual() {
    for fatal in [
        BackendEvent::Invalidated,
        BackendEvent::DeviceLost,
        BackendEvent::DeviceChanged,
    ] {
        let (h, mut eng, mut io) = start(FakeConfig::default());
        let mut ev = Vec::new();
        run(&h, &mut eng, &mut io, 1.0, &mut ev);
        assert!(!eng.calibration_stale());
        assert!(eng.stats().callbacks_in > 50);
        h.inject_event(fatal);
        run(&h, &mut eng, &mut io, 0.1, &mut ev);

        assert!(ev.contains(&EngineEvent::Backend(fatal)));
        assert!(ev.contains(&EngineEvent::Stopped(fatal)), "{fatal:?}");
        assert!(eng.calibration_stale());
        let st = eng.stats();
        assert!(st.device_lost && st.calibration_stale);
        assert!(!eng.poll(), "streams closed");
        assert!(!h.is_open(), "{fatal:?}: backend streams dropped");
        // The callbacks played silence from the event on; no more frames are counted.
        let frames = eng.stats().frames_out;
        run(&h, &mut eng, &mut io, 0.3, &mut ev);
        assert_eq!(eng.stats().frames_out, frames);

        // Reopen: new epoch, fresh handles, running again, calibration still stale.
        let (mut mix, _tap) = eng.reopen().unwrap();
        assert_eq!(eng.epoch(), 1);
        assert!(mix.send(MixCmd::Play {
            tag: 1,
            sound: SoundId::Click,
            at: 4800,
            gain: 0.3,
        }));
        let mut ev2 = Vec::new();
        run(&h, &mut eng, &mut io, 1.0, &mut ev2);
        assert!(ev2.contains(&EngineEvent::Reopened { epoch: 1 }));
        assert!(ev2.contains(&EngineEvent::Mix(MixEvent::Started { tag: 1, at: 4800 })));
        let last = ev2
            .iter()
            .rev()
            .find_map(|e| match e {
                EngineEvent::Clock(c) => Some(*c),
                _ => None,
            })
            .unwrap();
        assert_eq!(last.epoch(), 1);
        assert!(eng.calibration_stale());
        assert!(!eng.stats().device_lost);
        eng.clear_calibration_stale();
        assert!(!eng.calibration_stale());
        assert!(eng.stats().frames_out > frames);
        let _ = eng.stop();
    }
}

/// Runs `f` on a thread and fails if it does not finish in `secs` (a hang).
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs))
        .expect("timed out: hang")
}

#[test]
fn t15_device_invalidated_with_analysis_thread_does_not_hang() {
    within(30, || {
        let (h, mut eng, mut io) =
            start_with(FakeConfig::default(), EngineConfig::default(), Pump::Thread);
        let rt = h.spawn_realtime(20.0);
        let mut ev = Vec::new();
        let t0 = Instant::now();
        // Wait for the engine to be alive.
        while eng.stats().callbacks_in < 100 {
            assert!(t0.elapsed() < Duration::from_secs(10), "no callbacks");
            std::thread::sleep(Duration::from_millis(1));
        }
        h.inject_event(BackendEvent::Invalidated);
        let mut stopped = false;
        while !stopped {
            assert!(t0.elapsed() < Duration::from_secs(10), "no Stopped event");
            drain(&mut io, &mut ev);
            stopped = ev.iter().any(|e| matches!(e, EngineEvent::Stopped(_)));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!eng.poll());
        assert!(!h.is_open());
        assert!(eng.calibration_stale());
        // Reopen with the thread running, then stop.
        let (_m, _t) = eng.reopen().unwrap();
        while eng.stats().epoch != 1 || eng.stats().callbacks_in < 200 {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "no callbacks after reopen"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(rt);
        let st = eng.stop();
        drain(&mut io, &mut ev);
        assert!(ev.contains(&EngineEvent::Reopened { epoch: 1 }));
        assert!(!st.device_lost, "a successful reopen clears the lost flag");
        assert_eq!(st.epoch, 1);
    });
}

#[test]
fn full_event_ring_drops_and_counts_without_blocking() {
    let cfg = EngineConfig {
        event_ring: 8,
        cmd_ring: 4,
        in_ring: 8,
        out_stamp_ring: 4,
        ..EngineConfig::default()
    };
    let (h, mut eng, mut io) = start_with(FakeConfig::default(), cfg, Pump::Manual);
    // Command queue: 4 fit, 6 are refused and counted.
    let sent = (0..10)
        .filter(|&i| {
            io.mix.send(MixCmd::Play {
                tag: i,
                sound: SoundId::Click,
                at: 1000 * u64::from(i),
                gain: 0.1,
            })
        })
        .count();
    assert_eq!(sent, 4);
    assert_eq!(eng.stats().drops.mix_cmds, 6);

    let t0 = Instant::now();
    // 2 s of callbacks with nobody running the analysis step: the stamp queues overflow.
    h.pump(2.0);
    assert!(eng.stats().drops.out_stamps > 0);
    assert!(eng.stats().drops.in_events > 0);
    // Analysis runs but the client does not read: the event queue overflows.
    for _ in 0..200 {
        h.pump(0.01);
        eng.pump();
    }
    assert!(t0.elapsed() < Duration::from_secs(20), "something blocked");
    let st = eng.stats();
    assert!(st.drops.events > 0, "{st:?}");
    eprintln!("drops: {:?}", st.drops);

    // The client wakes up: the engine says what it lost.
    let mut ev = Vec::new();
    for _ in 0..4 {
        drain(&mut io, &mut ev);
        h.pump(0.01);
        eng.pump();
    }
    drain(&mut io, &mut ev);
    let sum = |ring: Ring| -> u64 {
        ev.iter()
            .filter_map(|e| match e {
                EngineEvent::Dropped { ring: r, n } if *r == ring => Some(u64::from(*n)),
                _ => None,
            })
            .sum()
    };
    let st = eng.stats();
    assert_eq!(sum(Ring::MixCmds), 6);
    assert!(sum(Ring::OutStamps) > 0 && sum(Ring::OutStamps) <= st.drops.out_stamps);
    assert!(sum(Ring::InEvents) > 0 && sum(Ring::InEvents) <= st.drops.in_events);
    assert!(sum(Ring::Events) > 0 && sum(Ring::Events) <= st.drops.events);
    let _ = eng.stop();
}

#[test]
fn tap_handle_records_the_microphone() {
    let (h, mut eng, mut io) = start(FakeConfig::default());
    io.tap.set(true);
    let samples: Arc<[f32]> = (0..4800)
        .map(|i| if i < 2400 { 0.25 } else { 0.0 })
        .collect::<Vec<f32>>()
        .into();
    h.play_external(0.2, samples, 1.0).unwrap();
    let mut ev = Vec::new();
    run(&h, &mut eng, &mut io, 0.6, &mut ev);
    let n = io.tap.available();
    assert!(n > 20_000, "{n}");
    let mut buf = vec![0.0f32; n];
    assert_eq!(io.tap.read(&mut buf), n);
    assert!(buf.iter().any(|&v| (v - 0.25).abs() < 1e-4));
    assert_eq!(io.tap.origin(), 0);
    io.tap.set(false);
    let _ = eng.stop();
}

#[test]
fn stop_flushes_the_open_onset_group() {
    let fs = 48_000.0;
    let (h, mut eng, mut io) = start(FakeConfig::default());
    let take = take_for(fs, 12, 11);
    let f0 = h
        .play_external(3.5, take.audio.clone().into(), 1.0)
        .unwrap() as f64;
    let onset = f0 + take.truth[3];
    let mut ev = Vec::new();
    // Stop 15 ms after the onset: the group is still open (it closes 40 ms after the
    // last peak), so only `stop` can deliver its Strum.
    while (h.next_frames().unwrap().0 as f64) < onset + 0.015 * fs {
        h.pump(0.005);
        eng.pump();
        drain(&mut io, &mut ev);
    }
    let near = |t: f64| (t - onset).abs() < 0.005 * fs;
    assert!(
        !strums(&ev).iter().any(|s| near(s.0)),
        "the strum must still be open"
    );
    let _ = eng.stop();
    drain(&mut io, &mut ev);
    let got = strums(&ev);
    assert!(
        got.iter().any(|s| near(s.0)),
        "stop must flush the open group; strums: {got:?}"
    );
}

#[test]
fn old_mixer_handles_refuse_after_reopen_and_stop() {
    let (h, mut eng, mut io) = start(FakeConfig::default());
    let mut ev = Vec::new();
    run(&h, &mut eng, &mut io, 0.1, &mut ev);
    let cmd = MixCmd::Play {
        tag: 1,
        sound: SoundId::Click,
        at: 99_999,
        gain: 0.1,
    };
    assert!(io.mix.send(cmd));
    let (mut fresh, _tap) = eng.reopen().unwrap();
    let before = eng.stats().drops.mix_cmds;
    assert!(!io.mix.send(cmd), "dead handle must refuse");
    assert_eq!(eng.stats().drops.mix_cmds, before + 1);
    assert!(fresh.send(cmd));
    let _ = eng.stop();
    assert!(!fresh.send(cmd), "handle of a stopped engine must refuse");
}

#[test]
fn callbacks_that_skip_still_publish_their_duration() {
    let (h, mut eng, mut io) = start(FakeConfig::default());
    h.inject_event(BackendEvent::DeviceChanged);
    let mut ev = Vec::new();
    // The fake keeps calling; every callback is halted and returns early.
    for _ in 0..100 {
        h.pump(0.005);
        drain(&mut io, &mut ev);
    }
    let st = eng.stats();
    assert_eq!(st.callbacks_out + st.callbacks_in, 0);
    assert!(st.skipped_out > 40 && st.skipped_in > 40, "{st:?}");
    assert!(st.worst_out_ns > 0 && st.worst_in_ns > 0, "{st:?}");
    eng.pump();
    let _ = eng.stop();
}

// ---------------------------------------------------------------- Pulso, unit A1

/// Reads every whole block waiting in the raw ring.
fn read_raw(raw: &mut RawOutput) -> Vec<(RawMark, Vec<f32>)> {
    let mut blocks = Vec::new();
    let mut buf = Vec::new();
    while let Some(mark) = raw.read_block(&mut buf) {
        assert_eq!(buf.len(), mark.frames as usize, "{mark:?}");
        blocks.push((mark, buf.clone()));
    }
    blocks
}

#[test]
fn raw_ring_addresses_every_sample_by_input_frame() {
    let (h, mut eng, mut io) = start(FakeConfig::default());
    let mut ev = Vec::new();

    // One reader per handle, and nothing in the ring until it is switched on.
    run(&h, &mut eng, &mut io, 0.1, &mut ev);
    let mut raw = io.tap.take_raw().expect("the raw ring, once");
    assert!(io.tap.take_raw().is_none(), "one reader per handle");
    assert!(read_raw(&mut raw).is_empty(), "off by default");

    // A sound in which every sample says where it is.
    let sound: Arc<[f32]> = (0..60_000)
        .map(|j| 0.25 + (j % 4096) as f32 / 16_384.0)
        .collect::<Vec<f32>>()
        .into();
    let f0 = h.play_external(0.3, sound.clone(), 1.0).unwrap();
    let on_at = h.next_frames().unwrap().0;
    io.tap.set_raw(true);
    io.tap.set(true); // the tap of the calibrator, at the same time
    run(&h, &mut eng, &mut io, 0.5, &mut ev);
    // The input device loses 960 frames in the middle of the sound. The frame counter
    // does not see them (it counts delivered frames): the sound skips 960 samples.
    let gap = h.next_frames().unwrap().0;
    assert!(gap > f0 + 4800 && gap < f0 + 40_000);
    assert!(h.inject_xrun(Side::In, 960));
    run(&h, &mut eng, &mut io, 0.6, &mut ev);

    let blocks = read_raw(&mut raw);
    assert_eq!(raw.dropped_blocks(), 0);
    assert!(blocks.len() >= 100, "{} blocks", blocks.len());
    let first = blocks[0].0;
    assert_eq!(first.in_first, on_at, "from the next callback on");
    assert!(on_at >= 3840 && on_at.is_multiple_of(480), "{on_at}");
    let mut next = first;
    let mut all = Vec::new();
    for (mark, samples) in &blocks {
        assert!(mark.frames >= 1 && mark.frames <= 1024, "{mark:?}");
        assert_eq!(
            (mark.in_first, mark.det_first),
            (next.in_first, next.det_first),
            "the blocks follow each other"
        );
        next.in_first += u64::from(mark.frames);
        next.det_first += u64::from(mark.frames);
        for (i, got) in samples.iter().enumerate() {
            let f = mark.in_first + i as u64;
            let j = (f + if f >= gap { 960 } else { 0 }).checked_sub(f0);
            let want = j
                .and_then(|j| sound.get(j as usize))
                .copied()
                .unwrap_or(0.0);
            assert!(
                (got - want).abs() < 1.0e-6,
                "input frame {f}: {got}, not {want}"
            );
        }
        all.extend_from_slice(samples);
    }
    assert_eq!(
        next.in_first,
        h.next_frames().unwrap().0,
        "up to the last callback"
    );
    assert!(all.iter().filter(|v| **v > 0.2).count() > 40_000);

    // The tap is independent and has the same samples, in detector time.
    let n = io.tap.available();
    let mut tap = vec![0.0f32; n];
    assert_eq!(io.tap.read(&mut tap), n);
    let at = (io.tap.origin() - first.det_first) as usize;
    assert!(
        n > 40_000 && at + n <= all.len(),
        "{n} from {at} of {}",
        all.len()
    );
    assert_eq!(tap[..], all[at..at + n]);
    io.tap.set(false);

    // Off: the callbacks stop feeding it.
    io.tap.set_raw(false);
    run(&h, &mut eng, &mut io, 0.1, &mut ev);
    read_raw(&mut raw);
    run(&h, &mut eng, &mut io, 0.3, &mut ev);
    assert!(read_raw(&mut raw).is_empty());

    // Nobody reads for longer than the ring holds (4 s): whole blocks are dropped and
    // counted, and what is read is still whole and still addressed.
    io.tap.set_raw(true);
    run(&h, &mut eng, &mut io, 5.0, &mut ev);
    let dropped = raw.dropped_blocks();
    assert!(dropped >= 50, "{dropped} blocks dropped");
    let kept = read_raw(&mut raw);
    let frames: u64 = kept.iter().map(|(m, _)| u64::from(m.frames)).sum();
    assert!(
        (180_000..=192_000).contains(&frames),
        "{frames} frames kept"
    );
    for (mark, _) in &kept {
        assert_eq!(
            mark.in_first,
            mark.det_first + (first.in_first - first.det_first)
        );
    }
    run(&h, &mut eng, &mut io, 0.1, &mut ev);
    let after = read_raw(&mut raw);
    assert!(!after.is_empty(), "the ring goes on after an overflow");
    assert_eq!(raw.dropped_blocks(), dropped);
    let last = after.last().unwrap().0;
    assert_eq!(
        last.in_first + u64::from(last.frames),
        h.next_frames().unwrap().0
    );

    // A reopen gives a new handle with a new ring; the old reader gets nothing more.
    let (_mix, mut tap2) = eng.reopen().unwrap();
    let mut raw2 = tap2.take_raw().expect("a new ring for the new streams");
    assert!(tap2.take_raw().is_none());
    tap2.set_raw(true);
    run(&h, &mut eng, &mut io, 0.2, &mut ev);
    assert!(read_raw(&mut raw).is_empty(), "the old ring is closed");
    let fresh = read_raw(&mut raw2);
    assert!(fresh.len() >= 15, "{} blocks", fresh.len());
    assert_eq!(
        fresh[0].0.in_first, 0,
        "the frame counter of the new streams"
    );
    let _ = eng.stop();
}

#[test]
fn host_clock_and_playhead_follow_the_streams() {
    let fc = FakeConfig::default();
    let origin_ns = fc.host_origin_ns;
    let (h, mut eng, mut io) = start(fc);
    let ph = eng.playhead();
    assert_eq!(eng.host_now_ns(), Some(h.now_ns()));
    assert_eq!((ph.last_out(), ph.epoch()), (None, 0));

    let mut ev = Vec::new();
    run(&h, &mut eng, &mut io, 1.0, &mut ev);
    assert_eq!(eng.host_now_ns(), Some(h.now_ns()));
    assert_eq!(h.now_ns(), origin_ns + 1_000_000_000);
    // The mark is the stamp of the last output block.
    let m = ph.last_out().expect("a block was played");
    assert_eq!(m.first + u64::from(m.frames), h.next_frames().unwrap().1);
    assert_eq!((m.frames, m.epoch), (480, 0));
    let plays_ns = origin_ns as f64 + h.truth().unwrap().out_time_s(m.first as f64) * 1.0e9;
    assert!((m.device_ns as f64 - plays_ns).abs() < 200.0e3, "{m:?}");
    assert!(
        m.callback_ns <= h.now_ns() && m.callback_ns < m.device_ns,
        "{m:?}"
    );

    // Halted callbacks (a device event the engine has not acted on yet) play silence
    // and publish nothing.
    h.inject_event(BackendEvent::DeviceChanged);
    for _ in 0..40 {
        h.pump(0.005);
    }
    assert!(eng.stats().skipped_out >= 15, "{:?}", eng.stats());
    let held = ph.last_out().expect("the last stamp stays");
    assert_eq!(held.first + u64::from(held.frames), eng.stats().frames_out);
    assert!(held.first >= m.first && held.first < h.next_frames().unwrap().1 - 480 * 10);

    // The streams are closed: no host clock; the stamp is still there.
    eng.pump();
    drain(&mut io, &mut ev);
    assert_eq!(eng.host_now_ns(), None);
    assert_eq!((ph.last_out(), ph.epoch()), (Some(held), 0));

    // New streams: a new epoch, the old stamp is hidden, the clock is back.
    let (_mix, _tap) = eng.reopen().unwrap();
    assert_eq!(eng.host_now_ns(), Some(h.now_ns()));
    assert_eq!((ph.last_out(), ph.epoch()), (None, 1));
    assert_eq!(eng.playhead().epoch(), 1, "one playhead for every epoch");
    run(&h, &mut eng, &mut io, 0.1, &mut ev);
    let m = ph.last_out().expect("a block of the new streams");
    assert_eq!(m.epoch, 1);
    assert_eq!(m.first + u64::from(m.frames), h.next_frames().unwrap().1);
    assert!(m.first < 9600, "{m:?}");

    // The handle outlives the engine.
    let _ = eng.stop();
    assert_eq!(ph.last_out(), Some(m));
}
