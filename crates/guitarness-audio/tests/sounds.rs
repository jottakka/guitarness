//! T3: spectrum, duration and onset-detector safety of the generated sounds.

use std::f64::consts::PI;

use guitarness_audio::{MixCmd, Mixer, SoundBank, SoundId};
use guitarness_dsp::{OnsetConfig, OnsetDetector, OnsetEvent};

/// In-place radix-2 FFT.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        for s in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((ang * k as f64).cos(), (ang * k as f64).sin());
                let (a, b) = (s + k, s + k + len / 2);
                let (tr, ti) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// Energy below `cut` Hz relative to the total, in dB.
fn low_ratio_db(x: &[f32], fs: f64, cut: f64) -> f64 {
    let n = 1 << 16;
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    for (i, v) in x.iter().enumerate() {
        re[i] = *v as f64;
    }
    fft(&mut re, &mut im);
    let (mut low, mut tot) = (0.0, 0.0);
    for k in 0..=n / 2 {
        let p = re[k] * re[k] + im[k] * im[k];
        tot += p;
        if (k as f64) * fs / (n as f64) < cut {
            low += p;
        }
    }
    10.0 * (low / tot).log10()
}

#[test]
fn click_spectrum_duration_and_level() {
    for fs in [44100.0, 48000.0] {
        let bank = SoundBank::new(fs);
        for id in [SoundId::Click, SoundId::Accent] {
            let x = bank.get(id);
            let db = low_ratio_db(x, fs, 6000.0);
            let ms = x.len() as f64 / fs * 1000.0;
            let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            eprintln!("fs={fs} {id:?}: below6k={db:.1} dB dur={ms:.2} ms peak={peak:.3}");
            assert!(db <= -40.0, "{id:?} at {fs}: {db} dB");
            assert!(ms <= 40.0);
            assert!(peak <= 0.5012, "peak {peak}");
        }
        let probe = bank.get(SoundId::Probe);
        assert!(!probe.is_empty());
        assert!(probe.iter().fold(0.0f32, |m, v| m.max(v.abs())) <= 0.5012);
    }
}

fn lcg(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// Returns (strums, heard, other) counted over the whole run.
fn run_metronome(fs: f64, bpm: f64, seconds: f64, noise: f32) -> (usize, usize, usize) {
    let period = fs * 60.0 / bpm;
    let (mut ctx, crx) = rtrb::RingBuffer::new(8);
    let (etx, mut erx) = rtrb::RingBuffer::new(4096);
    let mut mixer = Mixer::new(SoundBank::new(fs), crx, etx);
    ctx.push(MixCmd::Metronome {
        tag: 1,
        start: 4800,
        period,
        accent_every: 4,
        beats: 0,
        gain: 1.0,
    })
    .unwrap();
    let mut det = OnsetDetector::new(fs, OnsetConfig::default());
    let (mut strums, mut heard, mut other) = (0, 0, 0);
    let mut count = |e: OnsetEvent| match e {
        OnsetEvent::Strum(_) => strums += 1,
        OnsetEvent::Heard { .. } => heard += 1,
        #[allow(unreachable_patterns)]
        _ => other += 1,
    };
    let total = (seconds * fs) as u64;
    let mut seed = 12345u64;
    let mut buf = vec![0.0f32; 480];
    let mut pos = 0u64;
    while pos < total {
        mixer.render(&mut buf, 1, pos);
        for s in buf.iter_mut() {
            *s += noise * lcg(&mut seed);
        }
        det.process(&buf, &mut count);
        while erx.pop().is_ok() {}
        pos += 480;
    }
    det.flush(&mut count);
    (strums, heard, other)
}

#[test]
fn metronome_never_triggers_the_onset_detector() {
    let mut worst = 0;
    let mut failures = Vec::new();
    for fs in [44100.0, 48000.0] {
        for bpm in [60.0, 100.0, 133.3, 180.0] {
            for noise in [0.0, 0.003] {
                let (s, h, o) = run_metronome(fs, bpm, 120.0, noise);
                eprintln!("fs={fs} bpm={bpm} noise={noise}: strums={s} heard={h} other={o}");
                worst = worst.max(s + h + o);
                if (s, h, o) != (0, 0, 0) {
                    failures.push(format!("fs={fs} bpm={bpm} noise={noise}: {s}/{h}/{o}"));
                }
            }
        }
    }
    eprintln!("max onset events in one run: {worst}");
    assert!(failures.is_empty(), "strum/heard/other: {failures:#?}");
}

/// True peak by FFT zero-padding interpolation (8x), independent of the library code.
fn true_peak(x: &[f32]) -> f64 {
    let n = x.len().next_power_of_two() * 2;
    let m = n * 8;
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    for (i, v) in x.iter().enumerate() {
        re[i] = *v as f64;
    }
    fft(&mut re, &mut im);
    let mut r2 = vec![0.0; m];
    let mut i2 = vec![0.0; m];
    for k in 0..=n / 2 {
        let w = if k == n / 2 { 0.5 } else { 1.0 };
        r2[k] = re[k] * w;
        i2[k] = im[k] * w;
        if k > 0 && k < n / 2 {
            r2[m - k] = re[n - k];
            i2[m - k] = im[n - k];
        }
        if k == n / 2 {
            r2[m - k] = re[k] * w;
            i2[m - k] = -im[k] * w;
        }
    }
    // Inverse FFT through conjugation.
    i2.iter_mut().for_each(|v| *v = -*v);
    fft(&mut r2, &mut i2);
    r2.iter().fold(0.0f64, |a, v| a.max((v / n as f64).abs()))
}

#[test]
fn true_peak_below_minus_6_dbfs() {
    for fs in [44100.0, 48000.0] {
        let bank = SoundBank::new(fs);
        for id in [SoundId::Click, SoundId::Accent, SoundId::Probe] {
            let x = bank.get(id);
            let sp = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let tp = true_peak(x);
            eprintln!(
                "fs={fs} {id:?}: sample peak {sp:.3}, true peak {tp:.3} ({:.2} dBFS)",
                20.0 * tp.log10()
            );
            assert!(tp <= 0.501, "{id:?} at {fs}: true peak {tp}");
        }
    }
}
