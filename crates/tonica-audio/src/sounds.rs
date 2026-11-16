//! Sounds the M2 mixer plays: metronome click, accent click and the latency probe.
//!
//! Everything is generated once, at the output sample rate, in [`SoundBank::new`].
//! Nothing allocates afterwards.

use std::f64::consts::PI;

use tonica_dsp::latency::Probe;

/// True-peak level of the click, as a linear amplitude (about -8 dBFS).
const CLICK_PEAK: f32 = 0.40;
/// Peak level of the accent click (about -6.5 dBFS).
const ACCENT_PEAK: f32 = 0.47;
/// Peak level of the probe (about -6.9 dBFS). Callers lower it with the play gain.
const PROBE_PEAK: f32 = 0.45;
/// Click length in seconds (one Hann window).
const CLICK_SECONDS: f64 = 0.010;

/// Identifies one sound of the [`SoundBank`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundId {
    /// Metronome click: 8 kHz and 11 kHz partials under a 10 ms Hann window.
    Click,
    /// Accented click: 8, 11 and 14 kHz partials, louder, still above 6 kHz.
    Accent,
    /// Calibration probe: `tonica_dsp::latency::Probe::signal`, scaled to a safe peak.
    Probe,
}

/// All sounds, rendered at one sample rate.
#[derive(Clone, Debug)]
pub struct SoundBank {
    fs: f64,
    click: Vec<f32>,
    accent: Vec<f32>,
    probe: Vec<f32>,
}

fn partial_click(fs: f64, partials: &[(f64, f64)], peak: f32) -> Vec<f32> {
    let n = (CLICK_SECONDS * fs).round() as usize;
    let mut v: Vec<f32> = (0..n)
        .map(|k| {
            let t = k as f64 / fs;
            let w = 0.5 - 0.5 * (2.0 * PI * k as f64 / n as f64).cos();
            let s: f64 = partials
                .iter()
                .map(|&(f, a)| a * (2.0 * PI * f * t).sin())
                .sum();
            (w * s) as f32
        })
        .collect();
    scale_to_peak(&mut v, peak);
    v
}

/// Peak of the band-limited signal between samples: 8x oversampling with a Hann-windowed
/// sinc (32 taps each side). Construction time only; it allocates nothing but is slow.
fn true_peak(x: &[f32]) -> f32 {
    const OS: usize = 8;
    const HALF: i64 = 32;
    let mut peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs())) as f64;
    for n in -HALF..x.len() as i64 + HALF {
        for p in 1..OS {
            let t = n as f64 + p as f64 / OS as f64;
            let lo = ((t - HALF as f64).ceil() as i64).max(0);
            let hi = ((t + HALF as f64).floor() as i64).min(x.len() as i64 - 1);
            let mut y = 0.0f64;
            for k in lo..=hi {
                let d = t - k as f64;
                let sinc = if d == 0.0 {
                    1.0
                } else {
                    (PI * d).sin() / (PI * d)
                };
                let w = 0.5 + 0.5 * (PI * d / HALF as f64).cos();
                y += x[k as usize] as f64 * sinc * w;
            }
            peak = peak.max(y.abs());
        }
    }
    peak as f32
}

/// Scales `v` so its true peak (not just its sample peak) equals `peak`.
fn scale_to_peak(v: &mut [f32], peak: f32) {
    let m = true_peak(v);
    if m > 0.0 {
        let g = peak / m;
        v.iter_mut().for_each(|x| *x *= g);
    }
}

impl SoundBank {
    /// Generates every sound at output rate `fs` (Hz). Allocates; call it once at start.
    pub fn new(fs: f64) -> Self {
        let click = partial_click(fs, &[(8000.0, 1.0), (11000.0, 0.7)], CLICK_PEAK);
        let accent = partial_click(
            fs,
            &[(8000.0, 1.0), (11000.0, 1.0), (14000.0, 0.8)],
            ACCENT_PEAK,
        );
        let mut probe = Probe::new(fs).signal();
        scale_to_peak(&mut probe, PROBE_PEAK);
        Self {
            fs,
            click,
            accent,
            probe,
        }
    }

    /// Sample rate the bank was generated at.
    pub fn sample_rate(&self) -> f64 {
        self.fs
    }

    /// Samples of `id` (mono).
    pub fn get(&self, id: SoundId) -> &[f32] {
        match id {
            SoundId::Click => &self.click,
            SoundId::Accent => &self.accent,
            SoundId::Probe => &self.probe,
        }
    }
}
