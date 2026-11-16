//! McLeod Pitch Method detector: faithful port of `makeDetector` (legacy/web-demo/src/dsp.js).
//!
//! Numeric contract (for golden parity): the window and the NSDF are f32; energy,
//! autocorrelation sums, peak refinement and everything returned are f64, exactly as
//! JavaScript does it (Float32Array storage, double arithmetic).
//!
//! Two autocorrelation back-ends compute the same NSDF:
//! - [`AcfMode::Direct`]: O(W·L) double sums in the same order as JS (bit-faithful).
//! - [`AcfMode::Fft`]: O(N log N) via real FFT (f64). Differs from Direct by rounding only.

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex, num_complex::Complex};
use std::sync::Arc;

/// Maximum number of key maxima kept per frame (same as JS).
const MAX_KEYS: usize = 96;
/// A key maximum is accepted if it reaches K × the highest key maximum.
const K: f64 = 0.9;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Analysis {
    /// Fundamental in Hz (0 when no pitch).
    pub f: f64,
    /// Clarity: NSDF peak height at the chosen lag (0..1).
    pub c: f64,
    /// RMS of the whole window.
    pub rms: f64,
    /// RMS of the last `hop` samples (onset energy).
    pub rms_s: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcfMode {
    Direct,
    Fft,
}

struct FftAcf {
    n: usize,
    fwd: Arc<dyn RealToComplex<f64>>,
    inv: Arc<dyn ComplexToReal<f64>>,
    time: Vec<f64>,
    spec: Vec<Complex<f64>>,
    scratch_f: Vec<Complex<f64>>,
    scratch_i: Vec<Complex<f64>>,
}

pub struct Detector {
    fs: f64,
    w: usize,
    min_lag: usize,
    max_lag: usize,
    l: usize,
    nsdf: Vec<f32>,
    k_lag: [usize; MAX_KEYS],
    k_val: [f32; MAX_KEYS],
    pk_lag: f64,
    pk_val: f64,
    mode: AcfMode,
    fft: Option<FftAcf>,
    acf: Vec<f64>,
}

impl Detector {
    /// `fs`: analysis sample rate (already decimated). `w`: window length in samples.
    pub fn new(fs: f64, w: usize, mode: AcfMode) -> Self {
        let min_lag = 2usize.max((fs / 1450.0).floor() as usize);
        let max_lag = ((w >> 1) - 2).min((fs / 68.0).ceil() as usize);
        let l = max_lag + 1;
        let fft = match mode {
            AcfMode::Direct => None,
            AcfMode::Fft => {
                let n = (w + l + 1).next_power_of_two();
                let mut planner = RealFftPlanner::<f64>::new();
                let fwd = planner.plan_fft_forward(n);
                let inv = planner.plan_fft_inverse(n);
                let scratch_f = fwd.make_scratch_vec();
                let scratch_i = inv.make_scratch_vec();
                Some(FftAcf {
                    n,
                    time: vec![0.0; n],
                    spec: fwd.make_output_vec(),
                    fwd,
                    inv,
                    scratch_f,
                    scratch_i,
                })
            }
        };
        Self {
            fs,
            w,
            min_lag,
            max_lag,
            l,
            nsdf: vec![0.0; l + 2],
            k_lag: [0; MAX_KEYS],
            k_val: [0.0; MAX_KEYS],
            pk_lag: 0.0,
            pk_val: 0.0,
            mode,
            fft,
            acf: vec![0.0; l + 1],
        }
    }

    pub fn min_lag(&self) -> usize {
        self.min_lag
    }
    pub fn max_lag(&self) -> usize {
        self.max_lag
    }
    pub fn window(&self) -> usize {
        self.w
    }
    pub fn fs(&self) -> f64 {
        self.fs
    }

    fn refine(&mut self, i: usize) {
        let a = self.nsdf[i - 1] as f64;
        let b = self.nsdf[i] as f64;
        let c = self.nsdf[i + 1] as f64;
        let d = a - 2.0 * b + c;
        let mut sh = 0.0;
        if d < 0.0 {
            sh = 0.5 * (a - c) / d;
            sh = sh.clamp(-0.5, 0.5);
        }
        self.pk_lag = i as f64 + sh;
        self.pk_val = b - 0.25 * (a - c) * sh;
    }

    fn autocorr(&mut self, x: &[f32]) {
        let (w, l) = (self.w, self.l);
        match self.mode {
            AcfMode::Direct => {
                for tau in 0..=l {
                    let mut acf = 0.0f64;
                    for j in 0..(w - tau) {
                        acf += x[j] as f64 * x[j + tau] as f64;
                    }
                    self.acf[tau] = acf;
                }
            }
            AcfMode::Fft => {
                let f = self.fft.as_mut().expect("fft plan");
                for (d, s) in f.time.iter_mut().zip(x.iter()) {
                    *d = *s as f64;
                }
                f.time[w..].fill(0.0);
                f.fwd
                    .process_with_scratch(&mut f.time, &mut f.spec, &mut f.scratch_f)
                    .expect("fft fwd");
                for c in f.spec.iter_mut() {
                    *c = Complex::new(c.norm_sqr(), 0.0);
                }
                f.inv
                    .process_with_scratch(&mut f.spec, &mut f.time, &mut f.scratch_i)
                    .expect("fft inv");
                let scale = 1.0 / f.n as f64;
                for tau in 0..=l {
                    self.acf[tau] = f.time[tau] * scale;
                }
            }
        }
    }

    /// Analyse the last window. `x.len()` must equal the window length.
    pub fn analyze(&mut self, x: &[f32], hop_n: usize) -> Analysis {
        let w = self.w;
        debug_assert_eq!(x.len(), w);
        let mut m = 0.0f64;
        for &v in x {
            let v = v as f64;
            m += v * v;
        }
        let rms = (m / w as f64).sqrt();
        let hop_n = hop_n.min(w);
        let mut s = 0.0f64;
        for &v in &x[w - hop_n..] {
            let v = v as f64;
            s += v * v;
        }
        let rms_s = (s / hop_n as f64).sqrt();

        self.autocorr(x);
        let l = self.l;
        let mut mm = 2.0 * m;
        for tau in 0..=l {
            self.nsdf[tau] = if mm > 1e-9 {
                (2.0 * self.acf[tau] / mm) as f32
            } else {
                0.0
            };
            let a = x[tau] as f64;
            let b = x[w - 1 - tau] as f64;
            mm -= a * a + b * b;
        }
        self.nsdf[l + 1] = self.nsdf[l];

        // key maxima: the highest local maximum of each positive lobe
        let nsdf = &self.nsdf;
        let mut i = 1usize;
        let mut nk = 0usize;
        let mut best = 0.0f64;
        while i < l && nsdf[i] > 0.0 {
            i += 1;
        }
        while i < l {
            while i < l && nsdf[i] <= 0.0 {
                i += 1;
            }
            let mut mi: Option<usize> = None;
            let mut mv = 0.0f64;
            while i < l && nsdf[i] > 0.0 {
                let v = nsdf[i];
                if v as f64 > mv && v >= nsdf[i - 1] && v >= nsdf[i + 1] {
                    mv = v as f64;
                    mi = Some(i);
                }
                i += 1;
            }
            if let Some(mi) = mi
                && mi >= self.min_lag
                && mi <= self.max_lag
                && nk < MAX_KEYS
            {
                self.k_lag[nk] = mi;
                self.k_val[nk] = mv as f32;
                nk += 1;
                if mv > best {
                    best = mv;
                }
            }
        }
        let mut out = Analysis {
            f: 0.0,
            c: 0.0,
            rms,
            rms_s,
        };
        if nk > 0 && best > 0.3 {
            let th = best * K;
            for k in 0..nk {
                if self.k_val[k] as f64 >= th {
                    let lag = self.k_lag[k];
                    self.refine(lag);
                    out.f = self.fs / self.pk_lag;
                    out.c = self.pk_val;
                    break;
                }
            }
        }
        out
    }

    /// NSDF peak height within ±45 cents of lag `t0` (0 if no peak there).
    fn peak_near(&mut self, t0: f64) -> f64 {
        let mut lo = (t0 * 0.974).floor() as isize;
        let mut hi = (t0 * 1.026).ceil() as isize;
        if lo < 1 {
            lo = 1;
        }
        let lmax = self.l as isize - 1;
        if hi > lmax {
            hi = lmax;
        }
        let mut bi: Option<usize> = None;
        let mut bv = 0.0f64;
        let mut i = lo;
        while i <= hi {
            let u = i as usize;
            let v = self.nsdf[u];
            if v as f64 > bv && v >= self.nsdf[u - 1] && v >= self.nsdf[u + 1] {
                bv = v as f64;
                bi = Some(u);
            }
            i += 1;
        }
        let Some(bi) = bi else { return 0.0 };
        self.refine(bi);
        let cents = 1200.0 * (self.pk_lag / t0).ln() / std::f64::consts::LN_2;
        if (-45.0..=45.0).contains(&cents) {
            self.pk_val
        } else {
            0.0
        }
    }

    /// 0..1: how present the note of frequency `freq` is in the last analysed window.
    pub fn presence(&mut self, freq: f64) -> f64 {
        let t0 = self.fs / freq;
        let (min_lag, max_lag) = (self.min_lag as f64, self.max_lag as f64);
        if t0 > max_lag || t0 < min_lag {
            return 0.0;
        }
        let s1 = self.peak_near(t0);
        if s1 < 0.5 {
            return 0.0;
        }
        // if half or a third of the period explains as well, the real note is higher
        if t0 / 2.0 >= min_lag && self.peak_near(t0 / 2.0) > s1 - 0.05 {
            return 0.0;
        }
        if t0 / 3.0 >= min_lag && self.peak_near(t0 / 3.0) > s1 - 0.05 {
            return 0.0;
        }
        // if double the period explains much better, the real note is an octave below
        if 2.0 * t0 <= max_lag && self.peak_near(2.0 * t0) > s1 + 0.12 {
            return 0.0;
        }
        s1
    }
}
