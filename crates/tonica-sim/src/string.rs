//! Plucked string for the benches. Karplus-Strong with:
//! - a one-pole loop low-pass, so high partials die faster than the fundamental (as on a real
//!   guitar; the demo's two-point average barely damps them at 48 kHz);
//! - a pick-position comb and an optional soft (thumb) low-pass on the excitation;
//! - a raised-cosine attack ramp;
//! - damping at a given sample (re-strike or hand mute);
//! - a left-hand pitch change **without** a new excitation (the "no strum" regressions).
//!
//! Offline only: allocates.

use tonica_dsp::synth::Mulberry32;

#[derive(Clone, Copy, Debug)]
pub struct Pluck {
    pub freq: f64,
    /// Seconds for the fundamental to fall 60 dB.
    pub t60: f64,
    /// Loop low-pass pole (0 = bright, 0.7 = dark).
    pub damp: f64,
    /// Excitation low-pass pole (0 = pick, 0.8 = thumb).
    pub soft: f64,
    /// Pick position as a fraction of the string length.
    pub pick_pos: f64,
    /// Attack ramp, seconds.
    pub attack: f64,
    pub gain: f64,
}

impl Pluck {
    pub fn new(freq: f64) -> Self {
        Self {
            freq,
            t60: 3.0,
            damp: 0.4,
            soft: 0.2,
            pick_pos: 0.15,
            attack: 0.0,
            gain: 1.0,
        }
    }
}

struct Loop {
    buf: Vec<f64>,
    pos: usize,
    g: f64,
    a: f64,
    lp: f64,
    ap_in: f64,
    ap_out: f64,
}

impl Loop {
    fn tune(sr: f64, p: &Pluck, freq: f64) -> (usize, f64, f64) {
        let period = sr / freq;
        let lp_delay = p.damp / (1.0 - p.damp);
        let n = (period - lp_delay - 0.1).floor().max(2.0) as usize;
        let frac = period - lp_delay - n as f64;
        let a = (1.0 - frac) / (1.0 + frac);
        let g = 10f64.powf(-3.0 * period / (p.t60 * sr));
        (n, a, g)
    }
}

/// Render `len` samples of one pluck. `stop`: damp from that sample (5 ms fade, then silence).
/// `refrets`: at sample `.0` the left hand moves the pitch to `.1` Hz without plucking (in time
/// order; any number of changes).
pub fn render(
    sr: f64,
    p: &Pluck,
    len: usize,
    stop: Option<usize>,
    refrets: &[(usize, f64)],
    rng: &mut Mulberry32,
) -> Vec<f32> {
    let (n, a, g) = Loop::tune(sr, p, p.freq);
    // excitation: noise, optional thumb low-pass, pick-position comb, zero mean, peak 1
    let mut e = vec![0.0f64; n];
    let mut lp = 0.0;
    for v in e.iter_mut() {
        lp = (1.0 - p.soft) * (rng.next_f64() * 2.0 - 1.0) + p.soft * lp;
        *v = lp;
    }
    let m = ((p.pick_pos * n as f64).round() as usize).clamp(1, n - 1);
    let comb: Vec<f64> = (0..n).map(|i| e[i] - e[(i + n - m) % n]).collect();
    let mean = comb.iter().sum::<f64>() / n as f64;
    let pk = comb.iter().map(|v| (v - mean).abs()).fold(1e-12, f64::max);
    let mut lo = Loop {
        buf: comb.iter().map(|v| (v - mean) / pk).collect(),
        pos: 0,
        g,
        a,
        lp: 0.0,
        ap_in: 0.0,
        ap_out: 0.0,
    };

    let ramp = (p.attack * sr).round() as usize;
    let fade = (0.005 * sr) as usize;
    let mut out = vec![0.0f32; len];
    let mut next_refret = 0usize;
    for (i, o) in out.iter_mut().enumerate() {
        if let Some(&(at, f)) = refrets.get(next_refret)
            && i == at
        {
            next_refret += 1;
            // Same waveform, new length: no new energy, only a pitch change.
            let (n2, a2, g2) = Loop::tune(sr, p, f);
            let n1 = lo.buf.len();
            let nb: Vec<f64> = (0..n2)
                .map(|k| {
                    let x = lo.pos as f64 + k as f64 * n1 as f64 / n2 as f64;
                    let i0 = x.floor() as usize;
                    let fr = x - i0 as f64;
                    lo.buf[i0 % n1] * (1.0 - fr) + lo.buf[(i0 + 1) % n1] * fr
                })
                .collect();
            lo.buf = nb;
            lo.pos = 0;
            lo.a = a2;
            lo.g = g2;
        }
        let cur = lo.buf[lo.pos];
        lo.lp = (1.0 - p.damp) * cur + p.damp * lo.lp;
        let ap = lo.a * lo.lp + lo.ap_in - lo.a * lo.ap_out;
        lo.ap_in = lo.lp;
        lo.ap_out = ap;
        lo.buf[lo.pos] = ap * lo.g;
        lo.pos = (lo.pos + 1) % lo.buf.len();

        let mut v = cur * p.gain;
        if i < ramp {
            v *= 0.5 - 0.5 * (std::f64::consts::PI * i as f64 / ramp as f64).cos();
        }
        if let Some(s) = stop {
            if i >= s + fade {
                break;
            }
            if i >= s {
                v *= 1.0 - (i - s) as f64 / fade as f64;
            }
        }
        *o = v as f32;
    }
    out
}

/// One pluck whose pitch moves continuously (slide, bend, vibrato) with **no** new excitation:
/// the same loop as [`render`], read through a time-varying fractional delay (4-point
/// Lagrange, flat to well past 5 kHz, so the movement itself neither adds nor removes
/// brightness). `freq_at(i)` is the pitch in Hz at sample `i`; `p.freq` is ignored.
pub fn render_glide(
    sr: f64,
    p: &Pluck,
    len: usize,
    freq_at: impl Fn(usize) -> f64,
    rng: &mut Mulberry32,
) -> Vec<f32> {
    let lp_delay = p.damp / (1.0 - p.damp);
    let f0 = freq_at(0);
    let n = ((sr / f0).round() as usize).max(4);
    // excitation as in `render`
    let mut e = vec![0.0f64; n];
    let mut lp = 0.0;
    for v in e.iter_mut() {
        lp = (1.0 - p.soft) * (rng.next_f64() * 2.0 - 1.0) + p.soft * lp;
        *v = lp;
    }
    let m = ((p.pick_pos * n as f64).round() as usize).clamp(1, n - 1);
    let comb: Vec<f64> = (0..n).map(|i| e[i] - e[(i + n - m) % n]).collect();
    let mean = comb.iter().sum::<f64>() / n as f64;
    let pk = comb.iter().map(|v| (v - mean).abs()).fold(1e-12, f64::max);

    // delay line: line[w - d] is the sample written d samples ago
    let f_min = (0..len).step_by(16).map(&freq_at).fold(f0, f64::min);
    let cap = (sr / f_min).ceil() as usize + n + 8;
    let mut line = vec![0.0f64; cap];
    for (i, v) in comb.iter().enumerate() {
        line[i] = (v - mean) / pk;
    }
    let mut w = n; // next write position
    let mut lp_state = 0.0f64;
    let ramp = (p.attack * sr).round() as usize;
    let mut out = vec![0.0f32; len];
    for (i, o) in out.iter_mut().enumerate() {
        let period = sr / freq_at(i);
        let g = 10f64.powf(-3.0 * period / (p.t60 * sr));
        // total loop delay = read delay + low-pass delay = one period
        let d = (period - lp_delay).max(2.0);
        let di = d.floor() as usize;
        let fr = d - di as f64;
        // 4-point Lagrange around the read point (taps at delays di-1 .. di+2)
        let at = |k: usize| line[(w + cap - k) % cap];
        let (xm1, x0, x1, x2) = (at(di - 1), at(di), at(di + 1), at(di + 2));
        let c_m1 = -fr * (fr - 1.0) * (fr - 2.0) / 6.0;
        let c_0 = (fr + 1.0) * (fr - 1.0) * (fr - 2.0) / 2.0;
        let c_1 = -(fr + 1.0) * fr * (fr - 2.0) / 2.0;
        let c_2 = (fr + 1.0) * fr * (fr - 1.0) / 6.0;
        let cur = c_m1 * xm1 + c_0 * x0 + c_1 * x1 + c_2 * x2;
        lp_state = (1.0 - p.damp) * cur + p.damp * lp_state;
        line[w] = lp_state * g;
        w = (w + 1) % cap;
        let mut v = cur * p.gain;
        if i < ramp {
            v *= 0.5 - 0.5 * (std::f64::consts::PI * i as f64 / ramp as f64).cos();
        }
        *o = v as f32;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band_rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn decays_and_stops() {
        let sr = 48000.0;
        let mut r = Mulberry32::new(1);
        let mut p = Pluck::new(110.0);
        p.t60 = 1.0;
        let y = render(sr, &p, 48000, None, &[], &mut r);
        let early = band_rms(&y[0..4800]);
        let late = band_rms(&y[43200..48000]);
        // ≈ 60 dB/s on the fundamental; the high partials go faster
        assert!(late < early * 0.01, "early {early} late {late}");
        let z = render(sr, &p, 48000, Some(10000), &[], &mut r);
        assert!(z[10000 + 240..].iter().all(|v| *v == 0.0));
    }
}
