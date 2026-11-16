//! The per-block path must not allocate (PROMPT §4: a test must fail on allocation).
//! Run in debug builds: assert_no_alloc aborts the process if the closure allocates.
//! (The crate compiles its checker out of release builds, so this file does too.)
#![cfg(debug_assertions)]

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use guitarness_dsp::synth::{Mulberry32, ks_pluck};
use guitarness_dsp::{AcfMode, Event, OnsetConfig, OnsetDetector, OnsetEvent, Pipeline};

#[global_allocator]
static A: AllocDisabler = AllocDisabler;

#[test]
fn pipeline_process_does_not_allocate() {
    for mode in [AcfMode::Fft, AcfMode::Direct] {
        let sr = 48000.0;
        let mut r = Mulberry32::new(3);
        // allocation happens here, outside the checked region
        let mut x = vec![0.0f32; 48000];
        for (k, m) in [45, 50, 55, 59].iter().enumerate() {
            let tone = ks_pluck(sr, guitarness_dsp::mtof(*m as f64, 440.0), 0.25, || {
                r.next_f64()
            });
            let off = 2000 + k * 11000;
            for (i, v) in tone.iter().enumerate() {
                x[off + i] += v * 0.2;
            }
        }
        let mut pipe = Pipeline::demo(sr, mode, 256);
        let mut notes = 0usize;
        assert_no_alloc(|| {
            pipe.set_targets(&[50, 55], 0.0);
            for (b, block) in x.chunks(256).enumerate() {
                pipe.process(
                    block,
                    (b * 256) as f64 / sr,
                    |_| {},
                    |e| {
                        if matches!(e, Event::Note { .. }) {
                            notes += 1;
                        }
                    },
                );
            }
        });
        assert!(notes >= 3, "{mode:?}: expected notes, got {notes}");
    }
}

#[test]
fn onset_process_does_not_allocate() {
    for sr in [48000.0, 44100.0] {
        let mut r = Mulberry32::new(5);
        // Allocation happens here, outside the checked region. Eight plucks over room noise,
        // each ringing into the next, for 6 s: every reference of the detector is in use and
        // its 4 s noise window turns over.
        let mut x: Vec<f32> = (0..(6.0 * sr) as usize)
            .map(|_| (0.002 * (r.next_f64() * 2.0 - 1.0)) as f32)
            .collect();
        let n = 8;
        for k in 0..n {
            let midi = [45.0, 52.0, 57.0, 60.0][k % 4];
            let tone = ks_pluck(sr, guitarness_dsp::mtof(midi, 440.0), 0.9, || r.next_f64());
            let off = (0.3 * sr) as usize + k * (0.65 * sr) as usize;
            for (i, v) in tone.iter().enumerate() {
                x[off + i] += v * 0.3;
            }
        }
        let mut det = OnsetDetector::new(sr, OnsetConfig::default());
        let (mut strums, mut heard) = (0usize, 0usize);
        assert_no_alloc(|| {
            let mut count = |e: OnsetEvent| match e {
                OnsetEvent::Strum(_) => strums += 1,
                OnsetEvent::Heard { .. } => heard += 1,
            };
            for block in x.chunks(256) {
                det.process(block, &mut count);
            }
            det.flush(&mut count);
        });
        assert_eq!((strums, heard), (n, n), "{sr}");
    }
}
