//! Decimation by 2 with a linear-phase half-band FIR (windowed sinc, Blackman).
//! Replaces the demo's two-sample average on the onset path. No allocation per sample.

/// Number of taps (odd). Group delay = (TAPS - 1) / 2 input samples.
pub const TAPS: usize = 31;

pub struct Decimator2 {
    taps: [f32; TAPS],
    /// Doubled ring so the last TAPS samples are always contiguous.
    hist: [f32; 2 * TAPS],
    pos: usize,
    odd: bool,
}

impl Default for Decimator2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Decimator2 {
    pub fn new() -> Self {
        let mut taps = [0.0f32; TAPS];
        let m = (TAPS - 1) as f64;
        let fc = 0.25; // cutoff = fs_in / 4 (half-band)
        let mut sum = 0.0;
        for (i, t) in taps.iter_mut().enumerate() {
            let x = i as f64 - m / 2.0;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
            };
            let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / m).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / m).cos();
            *t = (sinc * w) as f32;
            sum += sinc * w;
        }
        for t in taps.iter_mut() {
            *t = (*t as f64 / sum) as f32;
        }
        Self {
            taps,
            hist: [0.0; 2 * TAPS],
            pos: 0,
            odd: false,
        }
    }

    /// Group delay in input samples.
    pub const fn delay_in() -> f64 {
        (TAPS - 1) as f64 / 2.0
    }

    /// Push one input sample; returns an output sample every second call.
    #[inline]
    pub fn push(&mut self, x: f32) -> Option<f32> {
        self.hist[self.pos] = x;
        self.hist[self.pos + TAPS] = x;
        self.pos += 1;
        if self.pos == TAPS {
            self.pos = 0;
        }
        self.odd = !self.odd;
        if self.odd {
            return None;
        }
        // hist[pos..pos+TAPS] = samples oldest → newest
        let w = &self.hist[self.pos..self.pos + TAPS];
        let mut acc = 0.0f32;
        for (a, b) in w.iter().zip(self.taps.iter().rev()) {
            acc += a * b;
        }
        Some(acc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain_at(f: f64) -> f64 {
        let sr = 48000.0;
        let mut d = Decimator2::new();
        let mut out = Vec::new();
        for n in 0..48000 {
            let x = (2.0 * std::f64::consts::PI * f * n as f64 / sr).sin() as f32;
            if let Some(y) = d.push(x) {
                out.push(y as f64);
            }
        }
        let tail = &out[2000..];
        (tail.iter().map(|v| v * v).sum::<f64>() / tail.len() as f64 * 2.0).sqrt()
    }

    #[test]
    fn passband_and_alias_band() {
        // passband for onset analysis (≤ 5 kHz) is flat
        for f in [100.0, 1000.0, 5000.0] {
            let g = gain_at(f);
            assert!((g - 1.0).abs() < 0.02, "{f} Hz gain {g}");
        }
        // content ≥ 19 kHz would alias below 5 kHz: must be strongly attenuated
        for f in [19000.0, 21000.0, 23000.0] {
            let g = gain_at(f);
            assert!(g < 0.003, "{f} Hz gain {g} (≥ -50 dB required)");
        }
    }
}
