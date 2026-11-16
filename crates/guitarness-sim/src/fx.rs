//! Room, microphone and interference models for the benches. Offline; allocates.

use guitarness_dsp::biquad::{Biquad, Kind};
use guitarness_dsp::synth::Mulberry32;

pub fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Pink noise (Paul Kellet's refined filter), unit RMS.
pub fn pink(len: usize, rng: &mut Mulberry32) -> Vec<f32> {
    let mut b = [0.0f64; 7];
    let mut out: Vec<f32> = (0..len)
        .map(|_| {
            let w = rng.next_f64() * 2.0 - 1.0;
            b[0] = 0.99886 * b[0] + w * 0.0555179;
            b[1] = 0.99332 * b[1] + w * 0.0750759;
            b[2] = 0.96900 * b[2] + w * 0.1538520;
            b[3] = 0.86650 * b[3] + w * 0.3104856;
            b[4] = 0.55000 * b[4] + w * 0.5329522;
            b[5] = -0.7616 * b[5] - w * 0.0168980;
            let v = b.iter().sum::<f64>() + w * 0.5362;
            b[6] = w * 0.115926;
            v as f32
        })
        .collect();
    let r = rms(&out).max(1e-12);
    out.iter_mut().for_each(|v| *v = (*v as f64 / r) as f32);
    out
}

/// Mains hum (fundamental + 2 harmonics), unit RMS.
pub fn hum(len: usize, sr: f64, f: f64) -> Vec<f32> {
    let w = 2.0 * std::f64::consts::PI * f / sr;
    let mut out: Vec<f32> = (0..len)
        .map(|n| {
            let t = n as f64 * w;
            (t.sin() + 0.5 * (2.0 * t).sin() + 0.3 * (3.0 * t).sin()) as f32
        })
        .collect();
    let r = rms(&out).max(1e-12);
    out.iter_mut().for_each(|v| *v = (*v as f64 / r) as f32);
    out
}

/// Metronome tick with all energy above 6 kHz (DESIGN §5: game sounds stay out of the
/// guitar band when there are no headphones). 10 ms Hann burst of 8 + 11 kHz.
pub fn click(sr: f64) -> Vec<f32> {
    let n = (0.010 * sr) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / sr;
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos();
            let s = (2.0 * std::f64::consts::PI * 8000.0 * t).sin()
                + 0.6 * (2.0 * std::f64::consts::PI * 11000.0 * t).sin();
            (w * s) as f32
        })
        .collect()
}

/// The click the game really plays (`guitarness-audio`, `SoundBank`): partials at 8 and 11 kHz
/// (accent: 8, 11 and 14 kHz) under a 10 ms Hann window, peak 0.40 (accent 0.47).
pub fn game_click(sr: f64, accent: bool) -> Vec<f32> {
    let (partials, peak): (&[(f64, f64)], f64) = if accent {
        (&[(8000.0, 1.0), (11000.0, 1.0), (14000.0, 0.8)], 0.47)
    } else {
        (&[(8000.0, 1.0), (11000.0, 0.7)], 0.40)
    };
    let n = (0.010 * sr).round() as usize;
    let v: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / sr;
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos();
            w * partials
                .iter()
                .map(|&(f, a)| a * (2.0 * std::f64::consts::PI * f * t).sin())
                .sum::<f64>()
        })
        .collect();
    let m = v.iter().fold(1e-12f64, |m, x| m.max(x.abs()));
    v.iter().map(|x| (x * peak / m) as f32).collect()
}

/// Laptop-mic response: 2nd-order high-pass at 150 Hz.
pub fn laptop_mic(x: &mut [f32], sr: f64) {
    let mut hp = Biquad::new(Kind::HighPass, 150.0, std::f64::consts::FRAC_1_SQRT_2, sr);
    for v in x.iter_mut() {
        *v = hp.tick(*v);
    }
}

/// Schroeder reverb (4 combs + 2 all-passes), `wet` mixed onto the dry signal.
pub fn reverb(x: &mut [f32], sr: f64, rt60: f64, wet: f64) {
    if wet <= 0.0 {
        return;
    }
    let combs_ms = [29.7, 37.1, 41.1, 43.7];
    let mut acc = vec![0.0f64; x.len()];
    for d_ms in combs_ms {
        let d = (d_ms * 1e-3 * sr) as usize;
        let g = 10f64.powf(-3.0 * (d as f64 / sr) / rt60);
        let mut buf = vec![0.0f64; d];
        let mut pos = 0;
        for (i, v) in x.iter().enumerate() {
            let y = buf[pos];
            buf[pos] = *v as f64 + g * y;
            pos = (pos + 1) % d;
            acc[i] += y * 0.25;
        }
    }
    for (d_ms, g) in [(5.0, 0.7), (1.7, 0.7)] {
        let d = (d_ms * 1e-3 * sr) as usize;
        let mut buf = vec![0.0f64; d];
        let mut pos = 0;
        for v in acc.iter_mut() {
            let b = buf[pos];
            let y = -g * *v + b;
            buf[pos] = *v + g * y;
            pos = (pos + 1) % d;
            *v = y;
        }
    }
    for (v, a) in x.iter_mut().zip(&acc) {
        *v = (*v as f64 + wet * a) as f32;
    }
}

/// Add `src` × `gain` into `dst` starting at `at`.
pub fn mix_at(dst: &mut [f32], src: &[f32], at: usize, gain: f64) {
    for (d, s) in dst.iter_mut().skip(at).zip(src) {
        *d += (*s as f64 * gain) as f32;
    }
}

/// Standard normal (Box-Muller).
pub fn gauss(rng: &mut Mulberry32) -> f64 {
    let u1 = rng.next_f64().max(1e-12);
    let u2 = rng.next_f64();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// Uniform in [a, b).
pub fn uni(rng: &mut Mulberry32, a: f64, b: f64) -> f64 {
    a + (b - a) * rng.next_f64()
}
