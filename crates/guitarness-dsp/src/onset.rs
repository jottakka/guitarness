//! Strum onset detection with ~1 ms timing (DESIGN §1.3 C0/C1, §1.5).
//!
//! Path: input → high-pass 60 Hz → half-band decimation ×2 (input ≥ 32 kHz) → two branches
//! at the analysis rate `fs_a`:
//!
//! - **Detection.** STFT (Hann, `n_fft`, `hop`), log-compressed magnitude, half-wave spectral
//!   flux on 70 Hz–5 kHz against the frame `lag` hops back, max-filtered ±1 bin (SuperFlux).
//!   Adaptive threshold `median + k·MAD` over the last 0.5 s, plus an absolute floor. A peak
//!   is confirmed one hop later (local maximum above threshold). A peak less than
//!   `group_gap_s` after the one before joins its group (≤ 180 ms); the group closes when no
//!   peak can join it any more. The flux says that the spectrum changed; it cannot say
//!   whether a hand struck the strings, and its 43 ms window cannot say when.
//! - **Excitation.** A new excitation is new *energy*, and a bright one: the high partials of
//!   a ringing string die first, a pick or nail attack is broadband. So a ringing chord is
//!   dull exactly where an attack is strong, and the higher the band, the weaker the ring
//!   that hides a soft re-strike. The branch measures, in 0.5 ms cells, the energy above each
//!   of [`BANDS`] cut-offs (2 to 4.25 kHz; all bands end at 5 kHz, 60 dB/oct: the game's
//!   clicks, at 8 kHz and above, are 70 dB down). The **rise** of a cell is the largest over
//!   the bands of
//!
//!   ```text
//!   10·log10( (W + F) / (R + F) )      dB
//!   ```
//!
//!   where `W` is the energy of the band in the 2 ms window that ends with the cell, and the
//!   reference `R` is the larger of:
//!   1. the largest 2 ms window of the 15 ms before that window (longer than one period of
//!      the low E string, so a ringing string's own waveform is not a rise);
//!   2. older windows, `mask_drop_db` down and decaying (what the room may still return of an
//!      earlier attack: an echo is not a strum).
//!
//!   The floor `F` stands on both sides. It is the sum of:
//!
//!   - `noise_k` × the noise floor of the band (the lowest 32 ms average of the last 4 s of
//!     live input; digital silence is skipped);
//!   - the energy above `guard_hz` in the last two windows, `guard_db` down: what the band
//!     filters may let through of a sound above the bands;
//!   - a fixed 1e-8 (−97 dBFS RMS over the window).
//!
//!   A sum, not one more term of the maximum: the rise then fades as the band nears its
//!   floor (a window `k` times the floor over a silent reference rises by `10·log10(1 + k)`,
//!   not by `10·log10(k)`), so the scatter of the background noise in a silent band, or a
//!   metronome click in a silent input, is not a rise, and there is no level at which the
//!   statistic jumps. With the floor inside the maximum, the ring of a single string crossed
//!   it both ways and counted as strums.
//!
//!   A flux group is a strum only if some cell in it rises by `rise_min_db`: a left-hand
//!   pitch change, a slide, a bend or a hand resting on the strings adds no energy and is
//!   rejected. The strum is dated to the **earliest** strong rise, i.e. the first string
//!   struck, not the loudest one.
//!
//! [`OnsetEvent::Heard`] is the early notice for visual feedback. It leaves as soon as a cell
//! rises by `heard_db` (0.5 to 2.5 ms after the attack reaches the cells, without waiting for
//! the flux), or, for a weaker attack, with the first flux peak. An attack that rises by
//! `heard_db` at least `group_gap_s` after the one before starts a new strum: the open flux
//! group closes there, so the new attack gets a notice and a `Strum` of its own; a closer one
//! is another string of the same strum. Every `Strum` directly follows its own `Heard`.
//!
//! An attack in the first 32 ms of a stream is not reported: the references (15 ms of
//! history, the 32 ms average behind the noise floor) are not full yet, and a rise measured
//! against them would be the start of the stream itself.
//!
//! Times are in **input samples** since the stream start (fractional), with the known filter
//! delays removed. Everything is allocated in [`OnsetDetector::new`]; `process` does not
//! allocate. Non-finite input samples count as zero and the rest is held to ±4.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::biquad::{Biquad, Kind};
use crate::resample::Decimator2;

/// Number of timing bands (each from its own cut-off up to `env_lp_hz`).
pub const BANDS: usize = 4;
/// Order of the common low-pass (cascaded biquads: 60 dB/oct).
const LP_SECTIONS: usize = 5;
/// The noise floor is the minimum over this many sub-windows.
const NOISE_SUBS: usize = 8;
/// The guard looks at the last two rise windows: the band filters delay and ring.
const GUARD_WINDOWS: usize = 2;

#[derive(Clone, Debug)]
pub struct OnsetConfig {
    /// STFT size at the analysis rate.
    pub n_fft: usize,
    /// Hop at the analysis rate.
    pub hop: usize,
    /// Flux band, Hz.
    pub f_lo: f64,
    pub f_hi: f64,
    /// Reference frame distance for the flux, in hops.
    pub lag: usize,
    /// `ln(1 + compress·|X|)`, with |X| = 1 for a full-scale sine.
    pub compress: f32,
    /// Threshold history length, seconds.
    pub thr_window_s: f64,
    /// `thr = median + thr_k·MAD`.
    pub thr_k: f32,
    /// Absolute threshold floor (flux units).
    pub thr_floor: f32,
    /// Attacks less than this apart belong to the same strum: flux peaks (by the end of
    /// their frames), and the strong rises that send the early `Heard`. Above the slowest
    /// strum (strings spread over 40 ms) by the time grid of the frames, one hop.
    pub group_gap_s: f64,
    /// Longest strum.
    pub group_max_s: f64,
    /// How far before the first flux peak the first attack may lie.
    pub search_back_s: f64,
    /// Lower edge of each timing band, Hz, ascending (fourth-order high-pass).
    pub band_hz: [f64; BANDS],
    /// Upper edge of every timing band, Hz (tenth-order low-pass).
    pub env_lp_hz: f64,
    /// Energy cell length, seconds.
    pub cell_s: f64,
    /// Rise window, in cells (4 × 0.5 ms = 2 ms).
    pub contrast_cells: usize,
    /// The reference is the largest window energy over this span before the window, seconds.
    pub hold_s: f64,
    /// Windows older than `hold_s` stay in the reference this far down, dB…
    pub mask_drop_db: f32,
    /// …and decay at this rate, dB per second.
    pub mask_decay_db_s: f32,
    /// This many times the noise floor of the band is added to the window and to its
    /// reference (see the module text: the floor `F`).
    pub noise_k: f32,
    /// Averaging time of the noise-floor estimate, seconds.
    pub noise_avg_s: f64,
    /// The noise floor is the lowest average over this span of live input, seconds.
    pub noise_window_s: f64,
    /// Sounds of the game lie above this frequency, Hz (fourth-order high-pass)…
    pub guard_hz: f64,
    /// …and this share of their energy, dB (≤ 0), is added to the floor `F` of every band.
    /// Must stay above what the band low-pass lets through (−71 dB at 8 kHz).
    pub guard_db: f32,
    /// Minimum rise, dB, for a strum to count.
    pub rise_min_db: f32,
    /// The onset is the first rise ≥ `rise_rel` × the largest rise in the group.
    pub rise_rel: f32,
    /// A rise this large sends `Heard` at once.
    pub heard_db: f32,
}

impl Default for OnsetConfig {
    fn default() -> Self {
        Self {
            n_fft: 1024,
            hop: 128,
            f_lo: 70.0,
            f_hi: 5000.0,
            lag: 2,
            compress: 1000.0,
            thr_window_s: 0.5,
            thr_k: 5.0,
            thr_floor: 0.03,
            group_gap_s: 0.045,
            group_max_s: 0.180,
            search_back_s: 0.060,
            band_hz: [2000.0, 2750.0, 3500.0, 4250.0],
            env_lp_hz: 5000.0,
            cell_s: 0.0005,
            contrast_cells: 4,
            hold_s: 0.015,
            mask_drop_db: 13.0,
            mask_decay_db_s: 100.0,
            noise_k: 4.0,
            noise_avg_s: 0.032,
            noise_window_s: 4.0,
            guard_hz: 6500.0,
            guard_db: -60.0,
            rise_min_db: 2.0,
            rise_rel: 0.4,
            heard_db: 4.0,
        }
    }
}

/// One strum (or single attack), final.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strum {
    /// Onset of the first attack, input samples.
    pub t: f64,
    /// First attack to last strong rise, input samples. Rough: hint use only.
    pub spread: f64,
    /// Flux peaks merged into this strum.
    pub peaks: u16,
    /// Largest flux / threshold in the group (≥ 1).
    pub strength: f32,
    /// Rise at the onset, dB.
    pub rise_db: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OnsetEvent {
    /// Early notice for visual feedback: a strong rise (at once) or the first flux peak of a
    /// group with a rise. `t` is the onset as known so far; `at` is the number of input
    /// samples consumed when it was emitted.
    ///
    /// Every `Strum` directly follows a `Heard` of its own: the event before a `Strum` is its
    /// `Heard`, and no two strums share one. A `Heard` whose flux group never forms or has no
    /// rise is not followed by a `Strum`.
    Heard { t: f64, at: f64 },
    /// Final event, emitted when the group closes: when no flux peak can join it any more
    /// (`group_gap_s`, rounded up to a hop, after the frame of its last peak), when an attack
    /// starts a new strum, or at `flush`.
    Strum(Strum),
}

/// Window-energy floor (about −97 dBFS RMS over 2 ms): digital silence is not an infinite
/// rise, and the edges of a game click in a silent input (where the guard has nothing to
/// hold on to yet) stay below 1 dB even at full scale.
const ENERGY_FLOOR: f64 = 1e-8;
/// A cell with less energy than this in the widest band (≈ −130 dBFS) is digital silence.
const SILENT_CELL: f64 = 1e-12;
/// Input samples are held to ±this (12 dB over full scale) and non-finite ones count as
/// zero, so a faulty driver cannot leave the filters with a state that never recovers. The
/// bound is low on purpose: what a glitch leaves in the references hides later attacks for a
/// time that grows by 10 ms per dB of its level.
const INPUT_LIMIT: f32 = 4.0;

#[derive(Clone, Copy, Debug)]
struct Retro {
    /// Cell of the earliest strong rise.
    cell: u64,
    onset_a: f64,
    rise_db: f32,
    spread_a: f64,
}

pub struct OnsetDetector {
    cfg: OnsetConfig,
    sr: f64,
    fs_a: f64,
    /// Input samples per analysis sample, and input time of analysis sample 0.
    ratio: f64,
    offset: f64,
    hp: Biquad,
    dec: Option<Decimator2>,

    // STFT
    ring: Vec<f32>,
    rpos: usize,
    win: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    fin: Vec<f32>,
    fout: Vec<Complex<f32>>,
    fscratch: Vec<Complex<f32>>,
    k_lo: usize,
    nb: usize,
    norm: f32,
    spec: Vec<f32>,
    spec_pos: usize,
    spec_count: usize,
    since_hop: usize,
    /// Input samples and analysis samples consumed.
    n_in: u64,
    n_a: u64,

    // threshold + peak picking
    fhist: Vec<f32>,
    fh_pos: usize,
    fh_count: usize,
    fsort: Vec<f32>,
    f1: f32,
    thr1: f32,
    end1: u64,
    f2: f32,
    frames: u64,

    // timing bands
    lp: [Biquad; LP_SECTIONS],
    bhp: [[Biquad; 2]; BANDS],
    /// Energy delay of the band filters (mean over the bands), analysis samples.
    env_delay_a: f64,
    cell: usize,
    cell_fill: usize,
    acc: [f64; BANDS],
    n_cells: u64,
    /// Last `avg_cells` cell energies of each band (ring, band-major).
    cell_hist: Vec<f64>,
    avg_cells: usize,
    /// Sum of each band's ring.
    avg_sum: [f64; BANDS],
    /// Last `win_len` window energies of each band (ring, band-major).
    win_hist: Vec<f64>,
    win_len: usize,
    hold_cells: usize,
    /// Forward mask of each band: older windows, dropped and decaying.
    mask: [f64; BANDS],
    mask_drop: f64,
    mask_decay: f64,
    /// Noise floor: lowest average of the current sub-window, of the closed ones, and those.
    nf_cur: [f64; BANDS],
    nf_old: [f64; BANDS],
    nf_subs: [[f64; NOISE_SUBS]; BANDS],
    nf_pos: usize,
    nf_fill: usize,
    nf_sub_cells: usize,
    /// Cells since the last digital silence (counted up to `avg_cells`).
    live: usize,
    /// Guard: energy above the bands in the last cells (ring), and its share that counts.
    ghp: [Biquad; 2],
    guard_acc: f64,
    guard_hist: Vec<f64>,
    guard_gain: f64,
    /// Rise of each cell, dB (ring over the last 0.5 s).
    rise: Vec<f32>,

    // grouping (analysis samples)
    gap_a: u64,
    max_a: u64,
    back_a: u64,
    g_active: bool,
    g_first: u64,
    g_last: u64,
    g_from: u64,
    g_peaks: u16,
    g_strength: f32,
    /// No later group searches before this (end of the last group, or start of the attack
    /// that the last early `Heard` announced).
    prev_end: u64,
    /// Strong rises: the last cell, and the first cell of the last run of them (an attack).
    strong_last: Option<u64>,
    attack: Option<u64>,
    /// Cell of the onset that the last `Heard` named, and whether its `Strum` has left.
    heard_cell: Option<u64>,
    heard_used: bool,
    rejected: u64,
    /// Diagnostic, debug builds with the `trace` feature only, switched on by `ONSET_DBG` in
    /// the environment when the detector is built: every group decision is written to
    /// stderr from `process`. Not for a real-time thread (stderr blocks, and allocates).
    #[cfg(all(feature = "trace", debug_assertions))]
    trace: bool,
}

/// Biquad Q values of a Butterworth filter of even order `2·n`, section `k` of `n`.
fn butterworth_q(k: usize, n: usize) -> f64 {
    1.0 / (2.0 * (std::f64::consts::PI * (2 * k + 1) as f64 / (4 * n) as f64).cos())
}

impl OnsetDetector {
    pub fn new(sr: f64, cfg: OnsetConfig) -> Self {
        assert!(cfg.n_fft.is_power_of_two() && cfg.hop > 0 && cfg.lag > 0);
        assert!(cfg.contrast_cells > 0);
        let decimate = sr >= 32000.0;
        let fs_a = if decimate { sr / 2.0 } else { sr };
        let (ratio, offset) = if decimate {
            // output m is produced at input index 2m+1 and centred TAPS/2 samples earlier
            (2.0, 1.0 - Decimator2::delay_in())
        } else {
            (1.0, 0.0)
        };
        let n = cfg.n_fft;
        let win: Vec<f32> = (0..n)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32)
            .collect();
        let wsum: f64 = win.iter().map(|&w| w as f64).sum();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let fin = fft.make_input_vec();
        let fout = fft.make_output_vec();
        let fscratch = fft.make_scratch_vec();
        let k_lo = ((cfg.f_lo * n as f64 / fs_a).ceil() as usize).max(1);
        let k_hi = ((cfg.f_hi * n as f64 / fs_a).floor() as usize).min(n / 2 - 1);
        let nb = k_hi + 1 - k_lo;
        let thr_len = ((cfg.thr_window_s * fs_a / cfg.hop as f64).round() as usize).max(8);

        let lp: [Biquad; LP_SECTIONS] = std::array::from_fn(|k| {
            Biquad::new(
                Kind::LowPass,
                cfg.env_lp_hz,
                butterworth_q(k, LP_SECTIONS),
                fs_a,
            )
        });
        let bhp: [[Biquad; 2]; BANDS] = std::array::from_fn(|b| {
            std::array::from_fn(|k| {
                Biquad::new(Kind::HighPass, cfg.band_hz[b], butterworth_q(k, 2), fs_a)
            })
        });
        let ghp: [Biquad; 2] = std::array::from_fn(|k| {
            Biquad::new(
                Kind::HighPass,
                cfg.guard_hz.min(0.45 * fs_a),
                butterworth_q(k, 2),
                fs_a,
            )
        });
        // Energy delay of the band filters: centroid of h² (a known constant, not fitted).
        // The bands differ by less than 0.1 ms, so one value serves all.
        let env_delay_a = {
            let mut lp = lp;
            let mut bhp = bhp;
            let (mut num, mut den) = ([0.0f64; BANDS], [0.0f64; BANDS]);
            for i in 0..4096 {
                let mut v = if i == 0 { 1.0f32 } else { 0.0 };
                for s in lp.iter_mut() {
                    v = s.tick(v);
                }
                for (b, f) in bhp.iter_mut().enumerate() {
                    let u = f[0].tick(v);
                    let y = f[1].tick(u);
                    let e = (y as f64) * (y as f64);
                    num[b] += i as f64 * e;
                    den[b] += e;
                }
            }
            (0..BANDS).map(|b| num[b] / den[b]).sum::<f64>() / BANDS as f64
        };
        let cell = ((cfg.cell_s * fs_a).round() as usize).max(1);
        let cells_of = |s: f64| ((s * fs_a) / cell as f64).round() as usize;
        let cap = ((0.5 * fs_a) / cell as f64).ceil() as usize;
        let hold_cells = cells_of(cfg.hold_s);
        let avg_cells = cells_of(cfg.noise_avg_s).max(cfg.contrast_cells);
        let win_len = cfg.contrast_cells + hold_cells + 2;
        let nf_sub_cells = (cells_of(cfg.noise_window_s) / NOISE_SUBS).max(1);
        let decay_per_cell = cfg.mask_decay_db_s as f64 * cell as f64 / fs_a;
        let sec = |s: f64| (s * fs_a).round() as u64;

        Self {
            hp: Biquad::new(Kind::HighPass, 60.0, std::f64::consts::FRAC_1_SQRT_2, sr),
            dec: decimate.then(Decimator2::new),
            ring: vec![0.0; n],
            rpos: 0,
            win,
            fft,
            fin,
            fout,
            fscratch,
            k_lo,
            nb,
            norm: (2.0 / wsum) as f32,
            spec: vec![0.0; (cfg.lag + 1) * nb],
            spec_pos: 0,
            spec_count: 0,
            since_hop: 0,
            n_in: 0,
            n_a: 0,
            fhist: vec![0.0; thr_len],
            fh_pos: 0,
            fh_count: 0,
            fsort: vec![0.0; thr_len],
            f1: 0.0,
            thr1: f32::INFINITY,
            end1: 0,
            f2: 0.0,
            frames: 0,
            lp,
            bhp,
            env_delay_a,
            cell,
            cell_fill: 0,
            acc: [0.0; BANDS],
            n_cells: 0,
            cell_hist: vec![0.0; BANDS * avg_cells],
            avg_cells,
            avg_sum: [0.0; BANDS],
            win_hist: vec![0.0; BANDS * win_len],
            win_len,
            hold_cells,
            mask: [0.0; BANDS],
            mask_drop: 10f64.powf(-(cfg.mask_drop_db as f64) / 10.0),
            mask_decay: 10f64.powf(-decay_per_cell / 10.0),
            nf_cur: [f64::INFINITY; BANDS],
            nf_old: [f64::INFINITY; BANDS],
            nf_subs: [[f64::INFINITY; NOISE_SUBS]; BANDS],
            nf_pos: 0,
            nf_fill: 0,
            nf_sub_cells,
            live: 0,
            ghp,
            guard_acc: 0.0,
            guard_hist: vec![0.0; GUARD_WINDOWS * cfg.contrast_cells],
            guard_gain: 10f64.powf(cfg.guard_db as f64 / 10.0),
            rise: vec![0.0; cap],
            gap_a: sec(cfg.group_gap_s),
            max_a: sec(cfg.group_max_s),
            back_a: sec(cfg.search_back_s),
            g_active: false,
            g_first: 0,
            g_last: 0,
            g_from: 0,
            g_peaks: 0,
            g_strength: 0.0,
            prev_end: 0,
            strong_last: None,
            attack: None,
            heard_cell: None,
            heard_used: false,
            rejected: 0,
            // read once here: nothing touches the environment on the audio path
            #[cfg(all(feature = "trace", debug_assertions))]
            trace: std::env::var_os("ONSET_DBG").is_some(),
            sr,
            fs_a,
            ratio,
            offset,
            cfg,
        }
    }

    pub fn sr(&self) -> f64 {
        self.sr
    }

    /// Analysis rate (Hz).
    pub fn fs_a(&self) -> f64 {
        self.fs_a
    }

    /// Flux groups dropped because no energy rise was found.
    pub fn rejected(&self) -> u64 {
        self.rejected
    }

    /// Input time of analysis time `a` (the delay of the decimator removed).
    #[inline]
    fn to_in(&self, a: f64) -> f64 {
        self.ratio * a + self.offset
    }

    pub fn process(&mut self, block: &[f32], mut on_event: impl FnMut(OnsetEvent)) {
        for &x in block {
            self.n_in += 1;
            let x = if x.is_finite() {
                x.clamp(-INPUT_LIMIT, INPUT_LIMIT)
            } else {
                0.0
            };
            let y = self.hp.tick(x);
            let a = match &mut self.dec {
                Some(d) => match d.push(y) {
                    Some(v) => v,
                    None => continue,
                },
                None => y,
            };
            self.push_analysis(a, &mut on_event);
        }
    }

    /// End of stream: take the flux peak that the next frame would have confirmed, and close
    /// the open group.
    pub fn flush(&mut self, mut on_event: impl FnMut(OnsetEvent)) {
        if self.frames >= 2 && self.f1 > self.thr1 && self.f1 >= self.f2 {
            let strength = self.f1 / self.thr1;
            self.on_peak(self.end1, strength, &mut on_event);
            // not a candidate again if the stream goes on after all
            self.thr1 = f32::INFINITY;
        }
        if self.g_active {
            self.close_group(&mut on_event);
        }
    }

    #[inline]
    fn push_analysis(&mut self, a: f32, on_event: &mut impl FnMut(OnsetEvent)) {
        let n = self.cfg.n_fft;
        self.ring[self.rpos] = a;
        self.rpos += 1;
        if self.rpos == n {
            self.rpos = 0;
        }
        let g = self.ghp[0].tick(a);
        let g = self.ghp[1].tick(g) as f64;
        self.guard_acc += g * g;
        let mut e = a;
        for s in self.lp.iter_mut() {
            e = s.tick(e);
        }
        for (acc, f) in self.acc.iter_mut().zip(self.bhp.iter_mut()) {
            let u = f[0].tick(e);
            let y = f[1].tick(u) as f64;
            *acc += y * y;
        }
        self.cell_fill += 1;
        self.n_a += 1;
        if self.cell_fill == self.cell {
            self.cell_fill = 0;
            self.end_cell(on_event);
        }
        self.since_hop += 1;
        if self.since_hop == self.cfg.hop {
            self.since_hop = 0;
            self.frame(on_event);
        }
    }

    /// Close an energy cell: update the references and store the cell's rise.
    fn end_cell(&mut self, on_event: &mut impl FnMut(OnsetEvent)) {
        let cc = self.cfg.contrast_cells;
        let (na, nw) = (self.avg_cells, self.win_len);
        let j = self.n_cells as usize;
        let (ja, jw) = (j % na, j % nw);
        // no rise while the filters and the references start up
        let warm = j >= na;
        // The noise floor is measured on a live input only: a full average without digital
        // silence (a muted microphone or a noise gate says nothing about the room).
        self.live = if self.acc[0] > SILENT_CELL {
            (self.live + 1).min(na)
        } else {
            0
        };
        let known = self.live >= na;
        let roll = known && self.nf_fill + 1 == self.nf_sub_cells;
        // floor: what the band filters may leak of the sound above the bands
        let ng = self.guard_hist.len();
        self.guard_hist[j % ng] = std::mem::take(&mut self.guard_acc);
        let guard = self.guard_gain * self.guard_hist.iter().sum::<f64>();
        let mut best = 0.0f64;
        for b in 0..BANDS {
            let cells = &mut self.cell_hist[b * na..(b + 1) * na];
            let e = std::mem::take(&mut self.acc[b]);
            self.avg_sum[b] += e - cells[ja];
            cells[ja] = e;
            if ja + 1 == na {
                // exact once per turn of the ring: no drift in the running sum
                self.avg_sum[b] = cells.iter().sum();
            }
            let w: f64 = (0..cc).map(|i| cells[(ja + na - i) % na]).sum();

            // reference 1: largest window ending in the `hold` cells before this window
            let wins = &mut self.win_hist[b * nw..(b + 1) * nw];
            let mut r = 0.0f64;
            for i in 0..=self.hold_cells {
                r = r.max(wins[(jw + 2 * nw - cc - i) % nw]);
            }
            // reference 2: the window that just left that span, dropped, and older ones decayed
            let left = wins[(jw + 2 * nw - cc - self.hold_cells - 1) % nw];
            self.mask[b] = (self.mask[b] * self.mask_decay).max(left * self.mask_drop);
            r = r.max(self.mask[b]);
            wins[jw] = w;

            // floor: the noise floor, in window units (none yet: only the fixed floor)
            if known {
                let avg = self.avg_sum[b].max(0.0) * cc as f64 / na as f64;
                self.nf_cur[b] = self.nf_cur[b].min(avg);
            }
            let nf = self.nf_cur[b].min(self.nf_old[b]);
            let nf = if nf.is_finite() { nf } else { 0.0 };
            let floor = self.cfg.noise_k as f64 * nf + guard + ENERGY_FLOOR;
            best = best.max((w + floor) / (r + floor));
            if roll {
                self.nf_subs[b][self.nf_pos] = self.nf_cur[b];
                self.nf_cur[b] = f64::INFINITY;
                self.nf_old[b] = self.nf_subs[b]
                    .iter()
                    .copied()
                    .fold(f64::INFINITY, f64::min);
            }
        }
        if known {
            self.nf_fill += 1;
            if roll {
                self.nf_fill = 0;
                self.nf_pos = (self.nf_pos + 1) % NOISE_SUBS;
            }
        }
        let rise = if warm {
            (10.0 * best.log10()) as f32
        } else {
            0.0
        };
        let cap = self.rise.len();
        self.rise[j % cap] = rise;
        self.n_cells += 1;

        if rise >= self.cfg.heard_db {
            self.strong_rise(j as u64, on_event);
        }
    }

    /// Cell `j` rose by `heard_db`. The first such cell of a run is an attack; an attack
    /// `group_gap_s` or more after the previous one is a new strum, and is announced at once.
    fn strong_rise(&mut self, j: u64, on_event: &mut impl FnMut(OnsetEvent)) {
        let cc = self.cfg.contrast_cells as u64;
        let last = self.strong_last.replace(j);
        if last.is_some_and(|l| j - l <= cc) {
            return; // the same attack, still filling the window
        }
        let cell = self.cell as u64;
        let new_strum = self.attack.is_none_or(|a| (j - a) * cell >= self.gap_a);
        self.attack = Some(j);
        if !new_strum {
            return; // a later string of the strum already announced
        }
        // The new strum ends the open group: what the flux finds from here on is its own.
        if self.g_active {
            self.close_group(on_event);
        }
        // ...and nothing as old as the previous strong rise can be part of it.
        if let Some(l) = last {
            self.prev_end = self.prev_end.max((l + 1) * cell);
        }
        // The attack began in the first cell of this climb (the window is not full yet).
        let cap = self.rise.len() as u64;
        let start = 0.5 * self.cfg.rise_min_db;
        let mut first = j;
        while first > 0 && j - first + 1 < cc && self.rise[((first - 1) % cap) as usize] >= start {
            first -= 1;
        }
        self.heard_cell = Some(j);
        self.heard_used = false;
        let t = (first as f64 + 0.5) * self.cell as f64 - self.env_delay_a;
        on_event(OnsetEvent::Heard {
            t: self.to_in(t),
            at: self.n_in as f64,
        });
    }

    /// Onset, in analysis samples, of the step that puts the largest rise in cell `j`: the
    /// first cell of that window, without the filter delay.
    fn onset_of(&self, j: u64) -> f64 {
        let j0 = (j + 1).saturating_sub(self.cfg.contrast_cells as u64);
        (j0 as f64 + 0.5) * self.cell as f64 - self.env_delay_a
    }

    fn frame(&mut self, on_event: &mut impl FnMut(OnsetEvent)) {
        let n = self.cfg.n_fft;
        let (head, tail) = self.ring.split_at(self.rpos);
        for ((d, s), w) in self
            .fin
            .iter_mut()
            .zip(tail.iter().chain(head.iter()))
            .zip(&self.win)
        {
            *d = s * w;
        }
        // Lengths are fixed at construction, so this cannot fail.
        let _ = self
            .fft
            .process_with_scratch(&mut self.fin, &mut self.fout, &mut self.fscratch);
        debug_assert_eq!(self.fout.len(), n / 2 + 1);

        let l1 = self.cfg.lag + 1;
        let nb = self.nb;
        let cur = self.spec_pos * nb;
        let lam = self.cfg.compress * self.norm;
        for j in 0..nb {
            self.spec[cur + j] = (1.0 + lam * self.fout[self.k_lo + j].norm()).ln();
        }
        let flux = if self.spec_count >= self.cfg.lag {
            let r = ((self.spec_pos + 1) % l1) * nb;
            let mut acc = 0.0f32;
            for j in 0..nb {
                let lo = j.saturating_sub(1);
                let hi = (j + 1).min(nb - 1);
                let mut m = self.spec[r + lo];
                for k in lo + 1..=hi {
                    m = m.max(self.spec[r + k]);
                }
                acc += (self.spec[cur + j] - m).max(0.0);
            }
            acc / nb as f32
        } else {
            0.0
        };
        self.spec_pos = (self.spec_pos + 1) % l1;
        self.spec_count += 1;

        let thr = self.threshold();
        let len = self.fhist.len();
        self.fhist[self.fh_pos] = flux;
        self.fh_pos = (self.fh_pos + 1) % len;
        self.fh_count = (self.fh_count + 1).min(len);

        let now = self.n_a;
        if self.frames >= 2 && self.f1 > self.thr1 && self.f1 >= self.f2 && self.f1 > flux {
            let strength = self.f1 / self.thr1;
            self.on_peak(self.end1, strength, on_event);
        }
        self.f2 = self.f1;
        self.f1 = flux;
        self.thr1 = thr;
        self.end1 = now;
        self.frames += 1;

        // The frame that ends now is the earliest that can still hold a peak: once it is a
        // gap after the last one, nothing can join the group.
        if self.g_active && now.saturating_sub(self.g_last) >= self.gap_a {
            self.close_group(on_event);
        }
    }

    fn threshold(&mut self) -> f32 {
        let c = self.fh_count;
        if c == 0 {
            return self.cfg.thr_floor;
        }
        let s = &mut self.fsort[..c];
        s.copy_from_slice(&self.fhist[..c]);
        let mid = c / 2;
        let med = *s.select_nth_unstable_by(mid, f32::total_cmp).1;
        for v in s.iter_mut() {
            *v = (*v - med).abs();
        }
        let mad = *s.select_nth_unstable_by(mid, f32::total_cmp).1;
        (med + self.cfg.thr_k * mad).max(self.cfg.thr_floor)
    }

    fn on_peak(&mut self, end: u64, strength: f32, on_event: &mut impl FnMut(OnsetEvent)) {
        if self.g_active
            && (end.saturating_sub(self.g_last) >= self.gap_a
                || end.saturating_sub(self.g_first) > self.max_a)
        {
            self.close_group(on_event);
        }
        if !self.g_active {
            self.g_active = true;
            self.g_first = end;
            self.g_from = end.saturating_sub(self.back_a).max(self.prev_end);
            self.g_peaks = 0;
            self.g_strength = 0.0;
        }
        self.g_last = end;
        self.g_peaks = self.g_peaks.saturating_add(1);
        self.g_strength = self.g_strength.max(strength);
        if !self.group_heard()
            && let Some(r) = self.retro(self.g_from, self.n_a)
        {
            self.heard(&r, on_event);
        }
    }

    /// The last `Heard` is this group's: no `Strum` has taken it, and the onset it named is
    /// not before the group's search span.
    fn group_heard(&self) -> bool {
        !self.heard_used
            && self
                .heard_cell
                .is_some_and(|h| h >= self.g_from / self.cell as u64)
    }

    /// The notice of an attack too weak for the early one, sent with its flux peak.
    fn heard(&mut self, r: &Retro, on_event: &mut impl FnMut(OnsetEvent)) {
        self.heard_cell = Some(r.cell);
        self.heard_used = false;
        // it counts as an attack: a strong string within the gap is the same strum
        if self.attack.is_none_or(|a| r.cell > a) {
            self.attack = Some(r.cell);
        }
        on_event(OnsetEvent::Heard {
            t: self.to_in(r.onset_a),
            at: self.n_in as f64,
        });
    }

    fn close_group(&mut self, on_event: &mut impl FnMut(OnsetEvent)) {
        self.g_active = false;
        self.prev_end = self.prev_end.max(self.g_last);
        match self.retro(self.g_from, self.g_last) {
            Some(r) => {
                if !self.group_heard() {
                    self.heard(&r, on_event);
                }
                self.heard_used = true;
                on_event(OnsetEvent::Strum(Strum {
                    t: self.to_in(r.onset_a),
                    spread: r.spread_a * self.ratio,
                    peaks: self.g_peaks,
                    strength: self.g_strength,
                    rise_db: r.rise_db,
                }))
            }
            None => self.rejected += 1,
        }
    }

    /// Earliest strong rise in the cells that lie inside analysis samples [from, to): from
    /// the cell that holds `from` to the last cell that ends by `to`. A cell counts only as a
    /// local maximum of the rise, judged with its real neighbours even when they lie outside
    /// the span: a rise still climbing at `to`, or already falling at `from`, belongs to the
    /// span next to this one. (A cell with no neighbour yet, at the present moment, counts.)
    fn retro(&self, from: u64, to: u64) -> Option<Retro> {
        let cap = self.rise.len() as u64;
        if self.n_cells == 0 {
            return None;
        }
        let newest = self.n_cells - 1;
        let oldest = self.n_cells.saturating_sub(cap);
        let cell = self.cell as u64;
        let j_to = (to / cell).checked_sub(1)?.min(newest);
        let j_from = (from / cell).max(oldest);
        if j_from > j_to {
            return None;
        }
        let r = |j: u64| self.rise[(j % cap) as usize];
        let rmax = (j_from..=j_to).map(r).fold(f32::NEG_INFINITY, f32::max);
        let base = self.cfg.rise_min_db;
        let tau = base.max(self.cfg.rise_rel * rmax);
        #[cfg(all(feature = "trace", debug_assertions))]
        if self.trace {
            eprint!(
                "retro from={from} to={to} j=[{j_from},{j_to}] base={base:.2} rmax={rmax:.2} tau={tau:.2}\n  R:"
            );
            for j in j_from..=j_to {
                eprint!(" {:.0}", r(j));
            }
            eprintln!();
        }
        if rmax < base {
            return None;
        }
        let is_peak = |j: u64, t: f32| {
            r(j) >= t && (j <= oldest || r(j) >= r(j - 1)) && (j >= newest || r(j) >= r(j + 1))
        };
        let j0 = (j_from..=j_to).find(|&j| is_peak(j, tau))?;
        let j1 = (j0 + 1..=j_to)
            .rev()
            .find(|&j| is_peak(j, base))
            .unwrap_or(j0);
        Some(Retro {
            cell: j0,
            onset_a: self.onset_of(j0),
            rise_db: r(j0),
            spread_a: ((j1 - j0) * cell) as f64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{Mulberry32, ks_pluck};

    fn render(sr: f64, onsets: &[usize], len: usize, seed: u32) -> Vec<f32> {
        let plucks: Vec<(usize, f32)> = onsets.iter().map(|&at| (at, 0.3)).collect();
        render_gains(sr, &plucks, len, seed)
    }

    /// Plucks as (first sample, gain), on exact silence.
    fn render_gains(sr: f64, plucks: &[(usize, f32)], len: usize, seed: u32) -> Vec<f32> {
        let mut r = Mulberry32::new(seed);
        let mut x = vec![0.0f32; len];
        for (i, &(at, gain)) in plucks.iter().enumerate() {
            let f = crate::mtof([45.0, 52.0, 57.0, 60.0][i % 4], 440.0);
            let tone = ks_pluck(sr, f, 0.6, || r.next_f64());
            for (k, v) in tone.iter().enumerate() {
                if at + k < len {
                    x[at + k] += gain * v;
                }
            }
        }
        x
    }

    /// Strum times, `Heard` count, and the latest `Heard` after its own onset (ms).
    fn run(sr: f64, x: &[f32]) -> (Vec<f64>, usize, f64) {
        let mut d = OnsetDetector::new(sr, OnsetConfig::default());
        let mut out = Vec::new();
        let mut heard = 0;
        let mut late = 0.0f64;
        for b in x.chunks(256) {
            d.process(b, |e| match e {
                OnsetEvent::Strum(s) => out.push(s.t),
                OnsetEvent::Heard { t, at } => {
                    heard += 1;
                    late = late.max((at - t) / sr * 1000.0);
                }
            });
        }
        d.flush(|e| {
            if let OnsetEvent::Strum(s) = e {
                out.push(s.t)
            }
        });
        (out, heard, late)
    }

    #[test]
    fn plucks_are_found_within_a_millisecond() {
        for sr in [48000.0, 44100.0] {
            let step = (0.5 * sr) as usize;
            let onsets: Vec<usize> = (0..6).map(|i| 4000 + i * step + i * 37).collect();
            let x = render(sr, &onsets, onsets[5] + step, 9);
            let (got, heard, late) = run(sr, &x);
            assert_eq!(got.len(), onsets.len(), "{sr}: {got:?}");
            assert_eq!(heard, onsets.len());
            // a clear attack is announced at once, not with the flux peak
            assert!(late < 5.0, "{sr}: Heard {late:.1} ms after the onset");
            for (g, &t) in got.iter().zip(&onsets) {
                let err_ms = (g - t as f64) / sr * 1000.0;
                assert!(err_ms.abs() < 1.0, "{sr}: err {err_ms:.3} ms");
            }
            // against the true attacks: one notice per strum, right before it in the
            // stream, and out within 4 ms of the attack itself
            let ev = events(sr, &x, 256);
            assert_eq!(kinds(&ev), "HS".repeat(onsets.len()), "{sr}: {ev:?}");
            assert_paired(&ev, sr);
            for (k, &t) in onsets.iter().enumerate() {
                let OnsetEvent::Heard { at, .. } = ev[2 * k] else {
                    unreachable!()
                };
                let late = (at - t as f64) / sr * 1000.0;
                assert!(
                    (0.0..4.0).contains(&late),
                    "{sr}: notice {k} left {late:.2} ms after its attack"
                );
            }
        }
    }

    #[test]
    fn silence_and_sustain_give_nothing_extra() {
        let sr = 48000.0;
        let (got, heard, _) = run(sr, &vec![0.0; 48000]);
        assert!(got.is_empty() && heard == 0);
        // one pluck, then let it ring for 3 s
        let x = render(sr, &[2000], 3 * 48000, 4);
        let (got, heard, _) = run(sr, &x);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(heard, 1);
    }

    #[test]
    fn game_clicks_in_a_silent_input_give_no_event() {
        // The metronome of the game: partials at 8 kHz and above under a 10 ms Hann window,
        // peak −6.5 dBFS, and exact zeros in between (muted microphone, loopback, noise gate).
        for sr in [48000.0, 44100.0] {
            let n = (0.010 * sr) as usize;
            let mut x = vec![0.0f32; (5.0 * sr) as usize];
            for k in 0..9 {
                let partials: &[(f64, f64)] = if k % 4 == 0 {
                    &[(8000.0, 1.0), (11000.0, 1.0), (14000.0, 0.8)]
                } else {
                    &[(8000.0, 1.0), (11000.0, 0.7)]
                };
                let click: Vec<f64> = (0..n)
                    .map(|i| {
                        let w =
                            0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos();
                        let ph = 2.0 * std::f64::consts::PI * i as f64 / sr;
                        w * partials
                            .iter()
                            .map(|&(f, a)| a * (f * ph).sin())
                            .sum::<f64>()
                    })
                    .collect();
                let peak = click.iter().fold(0.0f64, |m, v| m.max(v.abs()));
                let at = ((0.4 + 0.5 * k as f64) * sr) as usize;
                for (i, v) in click.iter().enumerate() {
                    x[at + i] = (0.47 * v / peak) as f32;
                }
            }
            let (got, heard, _) = run(sr, &x);
            assert!(
                got.is_empty() && heard == 0,
                "{sr}: {} strums, {heard} notices",
                got.len()
            );
        }
    }

    #[test]
    fn band_filters_keep_game_clicks_out() {
        // 8 kHz at full scale must stay ≥ 60 dB below a 4.6 kHz tone in every band
        let fs = 24000.0;
        let level = |f: f64| {
            let d = OnsetDetector::new(2.0 * fs, OnsetConfig::default());
            let (mut lp, mut bhp) = (d.lp, d.bhp);
            let mut e = [0.0f64; BANDS];
            for i in 0..4800 {
                let mut v = (2.0 * std::f64::consts::PI * f * i as f64 / fs).sin() as f32;
                for s in lp.iter_mut() {
                    v = s.tick(v);
                }
                for (b, f) in bhp.iter_mut().enumerate() {
                    let u = f[0].tick(v);
                    let y = f[1].tick(u) as f64;
                    if i >= 2400 {
                        e[b] += y * y;
                    }
                }
            }
            e
        };
        let (tone, click) = (level(4600.0), level(8000.0));
        for b in 0..BANDS {
            let db = 10.0 * (click[b] / tone[b]).log10();
            assert!(db < -60.0, "band {b}: {db:.1} dB");
        }
    }

    // ---- review regressions -------------------------------------------------------------

    fn events(sr: f64, x: &[f32], block: usize) -> Vec<OnsetEvent> {
        let mut d = OnsetDetector::new(sr, OnsetConfig::default());
        let mut out = Vec::new();
        for b in x.chunks(block) {
            d.process(b, |e| out.push(e));
        }
        d.flush(|e| out.push(e));
        out
    }

    fn ms_at(sr: f64, ms: f64) -> usize {
        (ms * 1e-3 * sr).round() as usize
    }

    /// How close a strum must be to its attack in these tests. On exact silence the pre-echo
    /// of the decimator stands 30 to 46 dB over the fixed floor, and the test string starts
    /// as a full-level noise burst: the attack is dated 0.8 to 1.2 ms early. (With any
    /// background noise the pre-echo is under the floor; the grids of guitarness-sim measure
    /// that case.)
    const EXACT_MS: f64 = 1.5;

    /// The contract of the two events: every `Strum` directly follows a `Heard` of its own
    /// (so no two strums share one), and that notice names an attack of the strum: on these
    /// clean signals, from one rise window before its onset to one after its last string.
    fn assert_paired(ev: &[OnsetEvent], sr: f64) {
        for (i, e) in ev.iter().enumerate() {
            let OnsetEvent::Strum(s) = e else { continue };
            let before = i.checked_sub(1).map(|k| ev[k]);
            let Some(OnsetEvent::Heard { t, at }) = before else {
                panic!("strum {i} does not follow a notice of its own: {ev:?}");
            };
            let ms = (t - s.t) / sr * 1000.0;
            let late = 2.0 + s.spread / sr * 1000.0;
            assert!(
                (-2.0..=late).contains(&ms),
                "notice {ms:+.1} ms from its strum: {ev:?}"
            );
            assert!(at >= t, "notice emitted before its own onset: {ev:?}");
        }
    }

    fn kinds(ev: &[OnsetEvent]) -> String {
        ev.iter()
            .map(|e| {
                if matches!(e, OnsetEvent::Strum(_)) {
                    'S'
                } else {
                    'H'
                }
            })
            .collect()
    }

    #[test]
    fn retro_stays_inside_its_span() {
        let mut d = OnsetDetector::new(48000.0, OnsetConfig::default());
        let c = d.cell as u64;
        d.n_cells = 400;
        // a rise in the cell that starts at the end of the span is not in the span
        d.rise[256] = 10.0;
        assert!(d.retro(2000, 256 * c).is_none());
        assert!(d.retro(256 * c, 4000).is_some());
        // a rise still climbing at the end of the span belongs to what comes after it
        d.rise[254] = 3.0;
        d.rise[255] = 6.0;
        d.rise[256] = 9.0;
        assert!(d.retro(2000, 256 * c).is_none());
        let r = d.retro(256 * c, 4000).unwrap();
        assert_eq!(r.onset_a, d.onset_of(256));
        // and one already falling at the start of the span belongs to what came before
        d.rise[254] = 0.0;
        d.rise[255] = 9.0;
        d.rise[256] = 6.0;
        d.rise[257] = 3.0;
        assert!(d.retro(256 * c, 4000).is_none());
        // nothing before the first cell
        assert!(d.retro(0, 5).is_none());
    }

    #[test]
    fn flux_peaks_a_gap_apart_are_two_groups() {
        // frames are one hop apart: the first distance of `group_gap_s` or more splits
        for sr in [48000.0, 44100.0] {
            let d = OnsetDetector::new(sr, OnsetConfig::default());
            let hop = d.cfg.hop as u64;
            let split = d.gap_a.div_ceil(hop);
            assert!((split - 1) * hop < d.gap_a && d.gap_a <= split * hop);
            for (hops, groups) in [(split - 1, 1u64), (split, 2)] {
                let mut d = OnsetDetector::new(sr, OnsetConfig::default());
                d.on_peak(1536, 2.0, &mut |_| {});
                d.on_peak(1536 + hops * hop, 2.0, &mut |_| {});
                d.flush(|_| {});
                assert_eq!(d.rejected(), groups, "{sr}: {hops} hops");
            }
        }
    }

    #[test]
    fn two_strums_never_share_a_notice() {
        // Two weak attacks (no early notice), 90 cells apart, each with its own flux group.
        // The notice of the first leaves when its flux peak is confirmed, i.e. after the
        // peak: it must not pass for the notice of the second group.
        let mut d = OnsetDetector::new(48000.0, OnsetConfig::default());
        let c = d.cell as u64;
        let mut ev = Vec::new();
        let at = |d: &mut OnsetDetector, n_a: u64| {
            d.n_a = n_a;
            d.n_in = 2 * n_a;
            d.n_cells = n_a / c;
        };
        d.rise[100] = 3.0;
        d.rise[190] = 3.0;
        at(&mut d, 1664);
        d.on_peak(1536, 2.0, &mut |e| ev.push(e));
        at(&mut d, 2688);
        d.close_group(&mut |e| ev.push(e));
        at(&mut d, 2944);
        d.on_peak(2816, 2.0, &mut |e| ev.push(e));
        at(&mut d, 3968);
        d.close_group(&mut |e| ev.push(e));
        assert_eq!(kinds(&ev), "HSHS", "{ev:?}");
        assert_paired(&ev, 48000.0);
    }

    #[test]
    fn a_roll_gets_one_notice() {
        // Three strings 25 ms apart, each louder than the ring of the one before (so each is
        // a strong rise of its own): one strum, one flash, dated at the first string.
        for sr in [48000.0, 44100.0] {
            let at = [ms_at(sr, 100.0), ms_at(sr, 125.0), ms_at(sr, 150.0)];
            let plucks = [(at[0], 0.03), (at[1], 0.1), (at[2], 0.3)];
            let x = render_gains(sr, &plucks, ms_at(sr, 700.0), 21);
            let ev = events(sr, &x, 256);
            assert_eq!(kinds(&ev), "HS", "{sr}: {ev:?}");
            assert_paired(&ev, sr);
            let OnsetEvent::Strum(s) = ev[1] else {
                unreachable!()
            };
            assert!(
                ((s.t - at[0] as f64) / sr * 1000.0).abs() < EXACT_MS,
                "{sr}: {s:?}"
            );
            assert!(s.peaks >= 2, "{sr}: {s:?}");
        }
    }

    #[test]
    fn attacks_a_gap_apart_get_an_early_notice_each() {
        // A soft attack and a full one `group_gap_s` + 1 ms later: two strums, and the second
        // is announced as fast as the first. (Two equal test strings are no use here: this
        // one rings as noise at full level, and a second of the same level adds under 2 dB.)
        let gap_ms = OnsetConfig::default().group_gap_s * 1000.0;
        for sr in [48000.0, 44100.0] {
            let at = [ms_at(sr, 100.0), ms_at(sr, 101.0 + gap_ms)];
            let x = render_gains(sr, &[(at[0], 0.03), (at[1], 0.3)], ms_at(sr, 700.0), 22);
            let ev = events(sr, &x, 256);
            assert_eq!(kinds(&ev), "HSHS", "{sr}: {ev:?}");
            assert_paired(&ev, sr);
            for (k, &onset) in at.iter().enumerate() {
                let (OnsetEvent::Heard { at: emitted, .. }, OnsetEvent::Strum(s)) =
                    (ev[2 * k], ev[2 * k + 1])
                else {
                    unreachable!()
                };
                let late = (emitted - onset as f64) / sr * 1000.0;
                assert!(
                    (0.0..4.0).contains(&late),
                    "{sr}: notice {k} {late:.1} ms after its attack"
                );
                assert!(
                    ((s.t - onset as f64) / sr * 1000.0).abs() < EXACT_MS,
                    "{sr}: {s:?}"
                );
            }
        }
    }

    #[test]
    fn notice_time_is_the_input_sample_count() {
        for sr in [48000.0, 44100.0] {
            let onsets = [ms_at(sr, 100.0), ms_at(sr, 400.0)];
            let x = render(sr, &onsets, ms_at(sr, 800.0), 23);
            let mut d = OnsetDetector::new(sr, OnsetConfig::default());
            // An early notice leaves when its cell closes, and names the middle of a cell at
            // most `contrast_cells − 1` cells back: `at − t` is one of that many values.
            let step = d.ratio * d.cell as f64;
            let nearest = d.ratio * (0.5 * d.cell as f64 + d.env_delay_a) - d.offset;
            let cc = d.cfg.contrast_cells as f64;
            let mut seen = 0;
            for (i, v) in x.iter().enumerate() {
                d.process(std::slice::from_ref(v), |e| {
                    if let OnsetEvent::Heard { t, at } = e {
                        seen += 1;
                        assert_eq!(
                            at,
                            (i + 1) as f64,
                            "{sr}: emitted while consuming sample {i}"
                        );
                        let cells = (at - t - nearest) / step;
                        assert!(
                            (cells - cells.round()).abs() < 1e-6
                                && (0.0..cc).contains(&cells.round()),
                            "{sr}: at − t = {} is {cells} cells over {nearest}",
                            at - t
                        );
                    }
                });
            }
            assert_eq!(seen, 2, "{sr}");
        }
    }

    #[test]
    fn non_finite_input_is_survived() {
        let sr = 48000.0;
        let onsets = [ms_at(sr, 100.0), ms_at(sr, 500.0), ms_at(sr, 900.0)];
        let mut x = render(sr, &onsets, ms_at(sr, 1400.0), 24);
        let bad = ms_at(sr, 300.0);
        x[bad] = f32::NAN;
        x[bad + 1] = f32::INFINITY;
        x[bad + 2] = f32::NEG_INFINITY;
        x[bad + 3] = f32::MAX;
        let ev = events(sr, &x, 256);
        let strums: Vec<f64> = ev
            .iter()
            .filter_map(|e| {
                if let OnsetEvent::Strum(s) = e {
                    Some(s.t)
                } else {
                    None
                }
            })
            .collect();
        // the glitch itself may count as an attack; the plucks after it must be found
        for &o in &onsets[1..] {
            assert!(
                strums
                    .iter()
                    .any(|t| ((t - o as f64) / sr * 1000.0).abs() < EXACT_MS),
                "{strums:?}"
            );
        }
        for e in &ev {
            match e {
                OnsetEvent::Strum(s) => {
                    assert!(s.t.is_finite() && s.rise_db.is_finite() && s.strength.is_finite())
                }
                OnsetEvent::Heard { t, at } => assert!(t.is_finite() && at.is_finite()),
            }
        }
    }

    #[test]
    fn flush_reports_a_strum_cut_short_by_the_end_of_the_stream() {
        // the stream ends 15 ms after an attack: its flux peak is still waiting for the next
        // frame to confirm it
        for sr in [48000.0, 44100.0] {
            let onset = ms_at(sr, 100.0);
            let x = render(sr, &[onset], ms_at(sr, 115.0), 25);
            let ev = events(sr, &x, 256);
            assert_eq!(kinds(&ev), "HS", "{sr}: {ev:?}");
            assert_paired(&ev, sr);
        }
    }

    #[test]
    fn nothing_is_reported_during_start_up() {
        // Known limit: for the first 32 ms the references (15 ms of history, the noise floor)
        // are not full, and no rise is measured. An attack in them is not reported.
        let sr = 48000.0;
        let onsets = [ms_at(sr, 5.0), ms_at(sr, 400.0)];
        let x = render(sr, &onsets, ms_at(sr, 900.0), 26);
        let ev = events(sr, &x, 256);
        assert_eq!(kinds(&ev), "HS", "{ev:?}");
        let OnsetEvent::Strum(s) = ev[1] else {
            unreachable!()
        };
        assert!(
            ((s.t - onsets[1] as f64) / sr * 1000.0).abs() < EXACT_MS,
            "{s:?}"
        );
    }
}
