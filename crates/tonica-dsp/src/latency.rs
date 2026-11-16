//! Loopback latency and player-bias calibration (DESIGN §1.5, §1.6 step 2).
//!
//! Offline: runs on the calibration recording after it ends, allocates freely, never on the
//! audio thread.
//!
//! **Loopback.** The game plays a probe (a 20 ms Hann-windowed linear chirp, 1.5–7 kHz)
//! `n` times; the microphone records. For each probe, a quadrature matched filter gives a
//! correlation envelope over the search window; the arrival is the **first** envelope peak
//! ≥ 50% of the largest one (the direct path: a reflection can be louder than it), refined
//! to a fraction of a sample. `L_rt` = median of arrival − emission. It is refused if fewer
//! than `min_found` probes are heard or their MAD exceeds 3 ms.
//!
//! **Player bias.** The player strums muted on the metronome. `e = (onset − L_rt) − beat`
//! for the strums that fall near a beat; `bias_player` = median(e). If the MAD exceeds 15 ms
//! (typical of Bluetooth headphones), rhythm stays visible but is not judged.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

#[derive(Clone, Debug)]
pub struct Probe {
    sr: f64,
    pub f0: f64,
    pub f1: f64,
    pub dur: f64,
    i: Vec<f64>,
    q: Vec<f64>,
}

impl Probe {
    pub fn new(sr: f64) -> Self {
        let (f0, f1, dur) = (1500.0, 7000.0, 0.020);
        let n = (dur * sr).round() as usize;
        let mut p = Self {
            sr,
            f0,
            f1,
            dur,
            i: Vec::with_capacity(n),
            q: Vec::with_capacity(n),
        };
        for k in 0..n {
            let (a, ph) = p.env_phase(k as f64 / sr);
            p.i.push(a * ph.cos());
            p.q.push(a * ph.sin());
        }
        p
    }

    fn env_phase(&self, t: f64) -> (f64, f64) {
        if !(0.0..self.dur).contains(&t) {
            return (0.0, 0.0);
        }
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * t / self.dur).cos();
        let ph = 2.0
            * std::f64::consts::PI
            * (self.f0 * t + (self.f1 - self.f0) * t * t / (2.0 * self.dur));
        (w, ph)
    }

    /// The probe as played (in-phase part), at the probe's sample rate.
    pub fn signal(&self) -> Vec<f32> {
        self.i.iter().map(|v| *v as f32).collect()
    }

    /// The probe evaluated at `t` seconds after its start (for fractional-delay tests).
    pub fn value_at(&self, t: f64) -> f64 {
        let (a, ph) = self.env_phase(t);
        a * ph.cos()
    }

    pub fn len(&self) -> usize {
        self.i.len()
    }

    pub fn is_empty(&self) -> bool {
        self.i.is_empty()
    }

    pub fn sr(&self) -> f64 {
        self.sr
    }
}

#[derive(Clone, Debug)]
pub struct LoopbackConfig {
    /// Longest round trip searched, seconds.
    pub search_max_s: f64,
    /// Envelope peak over its median, to count a probe as heard.
    pub min_peak_ratio: f64,
    pub min_found: usize,
    pub max_mad_ms: f64,
}

impl Default for LoopbackConfig {
    fn default() -> Self {
        Self {
            search_max_s: 0.5,
            min_peak_ratio: 8.0,
            min_found: 7,
            max_mad_ms: 3.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Loopback {
    /// Round-trip latency `L_rt`, samples.
    pub latency: f64,
    /// Median absolute deviation of the per-probe latencies, samples.
    pub mad: f64,
    pub found: usize,
    /// Per probe: measured latency, or `None` if not heard.
    pub per_probe: Vec<Option<f64>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CalError {
    TooFewProbes { found: usize, needed: usize },
    UnstableLatency { mad_ms: f64 },
    TooFewStrums { used: usize, needed: usize },
}

impl std::fmt::Display for CalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewProbes { found, needed } => {
                write!(f, "heard {found} probes, need {needed}")
            }
            Self::UnstableLatency { mad_ms } => {
                write!(f, "latency varies too much (MAD {mad_ms:.1} ms)")
            }
            Self::TooFewStrums { used, needed } => {
                write!(f, "{used} strums near a beat, need {needed}")
            }
        }
    }
}

impl std::error::Error for CalError {}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

fn median_mad(v: &[f64]) -> (f64, f64) {
    let mut a = v.to_vec();
    let m = median(&mut a);
    let mut d: Vec<f64> = v.iter().map(|x| (x - m).abs()).collect();
    (m, median(&mut d))
}

/// Measure `L_rt` from a recording. `emits[k]` is when probe `k` started playing, in the
/// recording's sample clock.
pub fn measure_loopback(
    rec: &[f32],
    emits: &[f64],
    probe: &Probe,
    cfg: &LoopbackConfig,
) -> Result<Loopback, CalError> {
    let sr = probe.sr();
    let p = probe.len();
    let span = (cfg.search_max_s * sr).ceil() as usize;
    let n = (span + 2 * p).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);

    let spectrum = |x: &[f64]| -> Vec<Complex<f64>> {
        let mut buf = vec![0.0; n];
        buf[..x.len()].copy_from_slice(x);
        let mut out = fwd.make_output_vec();
        fwd.process(&mut buf, &mut out).expect("fft length");
        out
    };
    let pi = spectrum(&probe.i);
    let pq = spectrum(&probe.q);

    let mut per_probe = Vec::with_capacity(emits.len());
    for &e in emits {
        let start = e.floor().max(0.0) as usize;
        if start >= rec.len() {
            per_probe.push(None);
            continue;
        }
        let end = (start + span + p).min(rec.len());
        let w: Vec<f64> = rec[start..end].iter().map(|v| *v as f64).collect();
        let x = spectrum(&w);
        let corr = |ps: &[Complex<f64>]| -> Vec<f64> {
            let mut c: Vec<Complex<f64>> = x.iter().zip(ps).map(|(a, b)| a * b.conj()).collect();
            let mut out = inv.make_output_vec();
            inv.process(&mut c, &mut out).expect("fft length");
            out
        };
        let ci = corr(&pi);
        let cq = corr(&pq);
        let lags = w.len().saturating_sub(p).min(span) + 1;
        let env: Vec<f64> = (0..lags)
            .map(|k| (ci[k] * ci[k] + cq[k] * cq[k]).sqrt())
            .collect();
        let (kmax, &emax) = env
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap_or((0, &0.0));
        let mut sorted = env.clone();
        let med = median(&mut sorted).max(1e-30);
        if emax < cfg.min_peak_ratio * med {
            per_probe.push(None);
            continue;
        }
        // direct path: first local maximum ≥ half the largest (ends compare one-sided)
        let k = (0..lags)
            .find(|&k| {
                env[k] >= 0.5 * emax
                    && (k == 0 || env[k] >= env[k - 1])
                    && (k + 1 == lags || env[k] >= env[k + 1])
            })
            .unwrap_or(kmax);
        let frac = if k > 0 && k + 1 < lags {
            let (a, b, c) = (env[k - 1], env[k], env[k + 1]);
            let den = a - 2.0 * b + c;
            if den.abs() > 1e-30 {
                (0.5 * (a - c) / den).clamp(-0.5, 0.5)
            } else {
                0.0
            }
        } else {
            0.0
        };
        per_probe.push(Some(start as f64 + k as f64 + frac - e));
    }

    let found: Vec<f64> = per_probe.iter().flatten().copied().collect();
    if found.len() < cfg.min_found {
        return Err(CalError::TooFewProbes {
            found: found.len(),
            needed: cfg.min_found,
        });
    }
    let (latency, mad) = median_mad(&found);
    let mad_ms = mad / sr * 1000.0;
    if mad_ms > cfg.max_mad_ms {
        return Err(CalError::UnstableLatency { mad_ms });
    }
    Ok(Loopback {
        latency,
        mad,
        found: found.len(),
        per_probe,
    })
}

#[derive(Clone, Debug)]
pub struct BiasConfig {
    /// A strum counts if it lies within this fraction of the beat interval from a beat.
    pub window_frac: f64,
    pub min_strums: usize,
    /// Above this MAD the player's rhythm is shown but not judged.
    pub judge_max_mad_ms: f64,
}

impl Default for BiasConfig {
    fn default() -> Self {
        Self {
            window_frac: 0.35,
            min_strums: 12,
            judge_max_mad_ms: 15.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerBias {
    /// `bias_player`, samples (positive = the player strums after the beat as heard).
    pub bias: f64,
    pub mad: f64,
    pub used: usize,
    /// False when the MAD is too large for rhythm judging (e.g. Bluetooth latency jitter).
    pub judgeable: bool,
}

/// `bias_player` from muted strums over the metronome. All times in input samples; `beats`
/// are the scheduled beat times mapped to the input clock, `latency` is `L_rt`.
pub fn player_bias(
    onsets: &[f64],
    beats: &[f64],
    latency: f64,
    sr: f64,
    cfg: &BiasConfig,
) -> Result<PlayerBias, CalError> {
    let mut ib: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let interval = if ib.is_empty() { sr } else { median(&mut ib) };
    let win = cfg.window_frac * interval;
    // One-to-one: closest (beat, strum) pairs first; each beat and each strum used once.
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (bi, &b) in beats.iter().enumerate() {
        for (oi, &o) in onsets.iter().enumerate() {
            let e = (o - latency) - b;
            if e.abs() <= win {
                pairs.push((e, bi, oi));
            }
        }
    }
    pairs.sort_by(|x, y| x.0.abs().total_cmp(&y.0.abs()));
    let mut beat_used = vec![false; beats.len()];
    let mut onset_used = vec![false; onsets.len()];
    let mut errs = Vec::new();
    for (e, bi, oi) in pairs {
        if !beat_used[bi] && !onset_used[oi] {
            beat_used[bi] = true;
            onset_used[oi] = true;
            errs.push(e);
        }
    }
    if errs.len() < cfg.min_strums {
        return Err(CalError::TooFewStrums {
            used: errs.len(),
            needed: cfg.min_strums,
        });
    }
    let (bias, mad) = median_mad(&errs);
    Ok(PlayerBias {
        bias,
        mad,
        used: errs.len(),
        judgeable: mad / sr * 1000.0 <= cfg.judge_max_mad_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::Mulberry32;

    fn gauss(r: &mut Mulberry32) -> f64 {
        let u1 = r.next_f64().max(1e-12);
        let u2 = r.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// Recording of `n` probes through a room: direct path at `lat_s` (+ per-probe jitter),
    /// echoes `(delay_s, gain)` after it, pink-ish noise at `snr_db` re the probe RMS.
    fn record(
        sr: f64,
        n: usize,
        lat_s: f64,
        jitter_s: f64,
        echoes: &[(f64, f64)],
        snr_db: f64,
        seed: u32,
    ) -> (Vec<f32>, Vec<f64>) {
        let probe = Probe::new(sr);
        let mut r = Mulberry32::new(seed);
        let gap = 0.6;
        let len = ((n as f64 * gap + 1.5) * sr) as usize;
        let mut x = vec![0.0f64; len];
        let mut emits = Vec::new();
        for k in 0..n {
            let e = (0.3 + k as f64 * gap) * sr + 0.37 * k as f64;
            emits.push(e);
            let d = lat_s + jitter_s * gauss(&mut r);
            for &(dt, g) in [(0.0, 1.0)].iter().chain(echoes) {
                let t0 = e / sr + d + dt;
                let a = (t0 * sr).floor() as usize;
                for (i, v) in x
                    .iter_mut()
                    .enumerate()
                    .skip(a.saturating_sub(1))
                    .take(probe.len() + 2)
                {
                    *v += g * probe.value_at(i as f64 / sr - t0);
                }
            }
        }
        let prms = (probe.i.iter().map(|v| v * v).sum::<f64>() / probe.len() as f64).sqrt();
        let nrms = prms * 10f64.powf(-snr_db / 20.0);
        let mut lp = 0.0;
        for v in x.iter_mut() {
            lp = 0.9 * lp + 0.1 * (r.next_f64() * 2.0 - 1.0);
            *v += nrms * lp * 4.0;
        }
        (x.iter().map(|v| *v as f32).collect(), emits)
    }

    #[test]
    fn latency_through_a_room_within_a_fifth_of_a_millisecond() {
        for sr in [48000.0, 44100.0] {
            let echoes = [(0.003, 0.6), (0.007, 0.45), (0.013, 0.3), (0.021, 0.2)];
            let (rec, emits) = record(sr, 10, 0.0873, 0.0, &echoes, 0.0, 3);
            let lb = measure_loopback(&rec, &emits, &Probe::new(sr), &LoopbackConfig::default())
                .unwrap();
            let err_ms = (lb.latency / sr - 0.0873) * 1000.0;
            assert!(err_ms.abs() < 0.2, "{sr}: err {err_ms:.3} ms");
            assert_eq!(lb.found, 10);
        }
    }

    #[test]
    fn louder_reflection_does_not_pull_the_estimate() {
        let sr = 48000.0;
        let (rec, emits) = record(sr, 10, 0.120, 0.0, &[(0.0025, 1.4)], 10.0, 5);
        let lb =
            measure_loopback(&rec, &emits, &Probe::new(sr), &LoopbackConfig::default()).unwrap();
        let err_ms = (lb.latency / sr - 0.120) * 1000.0;
        assert!(err_ms.abs() < 0.2, "err {err_ms:.3} ms");
    }

    #[test]
    fn arrival_at_lag_zero_is_the_direct_path() {
        // Review finding: zero delay + 0.6× echo at 2.5 ms used to report the echo.
        let sr = 48000.0;
        let (rec, emits) = record(sr, 10, 0.0, 0.0, &[(0.0025, 0.6)], 30.0, 13);
        let lb =
            measure_loopback(&rec, &emits, &Probe::new(sr), &LoopbackConfig::default()).unwrap();
        let err_ms = lb.latency / sr * 1000.0;
        assert!(err_ms.abs() < 0.2, "err {err_ms:.3} ms");
    }

    #[test]
    fn one_strum_never_counts_for_two_beats() {
        // Review finding: beats at 0 and 0.1 s, one strum at 0.05 s used to fill both.
        let sr = 1000.0;
        let mut beats = vec![0.0, 100.0];
        beats.extend((1..=10).map(|k| 100.0 + 500.0 * k as f64));
        let mut onsets = vec![50.0];
        onsets.extend((1..=10).map(|k| 100.0 + 500.0 * k as f64));
        let r = player_bias(&onsets, &beats, 0.0, sr, &BiasConfig::default());
        assert!(
            matches!(
                r,
                Err(CalError::TooFewStrums {
                    used: 11,
                    needed: 12
                })
            ),
            "{r:?}"
        );
    }

    #[test]
    fn jittery_or_silent_loopback_is_refused() {
        let sr = 48000.0;
        let (rec, emits) = record(sr, 10, 0.090, 0.008, &[], 20.0, 7);
        let r = measure_loopback(&rec, &emits, &Probe::new(sr), &LoopbackConfig::default());
        assert!(matches!(r, Err(CalError::UnstableLatency { .. })), "{r:?}");
        let (rec, emits) = record(sr, 10, 0.090, 0.0, &[], -40.0, 7);
        let r = measure_loopback(&rec, &emits, &Probe::new(sr), &LoopbackConfig::default());
        assert!(matches!(r, Err(CalError::TooFewProbes { .. })), "{r:?}");
    }

    #[test]
    fn player_bias_and_judgeability() {
        let sr = 48000.0;
        let lat = 0.090 * sr;
        let mut r = Mulberry32::new(11);
        let beats: Vec<f64> = (0..32).map(|k| (1.0 + k as f64 * 0.5) * sr).collect();
        let onsets: Vec<f64> = beats
            .iter()
            .map(|b| b + lat + (0.023 + 0.005 * gauss(&mut r)) * sr)
            .collect();
        let pb = player_bias(&onsets, &beats, lat, sr, &BiasConfig::default()).unwrap();
        assert!((pb.bias / sr - 0.023).abs() < 0.002, "{pb:?}");
        assert!(pb.judgeable);
        // Bluetooth-like jitter: shown, not judged
        let onsets: Vec<f64> = beats
            .iter()
            .map(|b| b + lat + (0.040 + 0.030 * gauss(&mut r)) * sr)
            .collect();
        let pb = player_bias(&onsets, &beats, lat, sr, &BiasConfig::default()).unwrap();
        assert!(!pb.judgeable, "{pb:?}");
        // too few strums near beats
        let r = player_bias(&onsets[..5], &beats, lat, sr, &BiasConfig::default());
        assert!(matches!(r, Err(CalError::TooFewStrums { .. })));
    }
}
