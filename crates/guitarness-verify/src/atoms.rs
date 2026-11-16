//! Dictionary of the DSP witness (the verifier/learner design plan section 2.4, second part).
//!
//! An atom is a pitch. This cut has no per-guitar calibration (`B_s = 0`, generic envelope),
//! so a (string, fret) pair is nothing but its note: two strings fretted to the same note
//! share one atom and cannot be told apart by this dictionary.
//!
//! An atom is a comb: one column per partial `h · f0` inside the band, each column being
//! the magnitude of the analysis window's transform (a Hann window of `win` samples,
//! zero-padded to [`N_FFT`]) centred on the partial. Every partial has its own gain in the
//! fit. The plan asked for two columns per atom with fixed `1/h` amplitudes inside each;
//! measured on the bench, that template is wrong by several dB per partial and any atom
//! sharing partials with the chord soaks up the misfit (false `Extra` on 7% of correct
//! strums). With free partial gains, only partials that no other atom has can speak for an
//! atom, which is also the physical limit: an octave-doubled string has none.
//!
//! A real guitar is never exactly at its reference pitch. An atom can be built off its
//! nominal pitch by some cents ([`Dict::add_detuned`]) and moved in place ([`Dict::retune`]);
//! the witness measures the offset of each strum before it fits (see [`crate::excite`]).
//!
//! The dictionary is rebuilt for every event inside storage allocated by [`Dict::new`];
//! nothing here allocates after that.

use guitarness_dsp::mtof;

/// FFT length of the analysis frame, samples at the decimated rate.
pub const N_FFT: usize = 4096;
/// Bins of a real FFT of [`N_FFT`] samples: the widest band a [`Grid`] can have.
pub const MAX_BINS: usize = N_FFT / 2 + 1;
/// Distinct pitches one event can hold (expected plus every alternative).
pub const MAX_ATOMS: usize = 48;
/// Partials kept per atom (a 78 Hz string has 64 below 5 kHz).
pub const MAX_PARTIALS: usize = 64;
/// Widest column, FFT bins. Sets the shortest window: see [`Grid::new`].
pub const MAX_LOBE: usize = 40;

/// The window transform is kept out to this many bins of the window length (the main lobe
/// ends at 2; the side lobes beyond 4 are more than 40 dB down).
const LOBE_HALF: f64 = 4.0;
/// Table steps per bin of the window length.
const LOBE_STEPS: usize = 128;
const OVERLAP_STEPS: usize = 32;
/// Lowest fundamental an atom may have, Hz (well below any guitar string).
const MIN_F0: f64 = 30.0;

/// Magnitude of the Hann window transform at `x` bins of the window length from the
/// partial, peak 1: `|sinc(x) / (1 - x²)|`.
pub fn hann_lobe(x: f64) -> f64 {
    let x = x.abs();
    if x < 1e-9 {
        return 1.0;
    }
    if (x - 1.0).abs() < 1e-6 {
        return 0.5;
    }
    let px = std::f64::consts::PI * x;
    (px.sin() / (px * (1.0 - x * x))).abs()
}

/// Band and resolution of one analysis frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    /// Sample rate of the analysed (decimated) signal, Hz.
    pub fs: f64,
    /// Hann length, samples (at most [`N_FFT`]); the rest of the frame is zero padding.
    pub win: usize,
    /// First FFT bin of the band.
    pub k0: usize,
    /// Number of bins in the band.
    pub nb: usize,
}

impl Grid {
    /// `None` when the band `f_lo..f_hi` does not fit below the Nyquist rate, or the window
    /// is longer than the frame or so short that a column would not fit in [`MAX_LOBE`] bins.
    pub fn new(fs: f64, win: usize, f_lo: f64, f_hi: f64) -> Option<Self> {
        let sane = fs.is_finite() && f_lo.is_finite() && f_hi.is_finite();
        if !sane || fs <= 0.0 || f_lo <= 0.0 || f_hi <= f_lo || win == 0 || win > N_FFT {
            return None;
        }
        if 2.0 * LOBE_HALF * N_FFT as f64 / win as f64 + 2.0 > MAX_LOBE as f64 {
            return None;
        }
        let df = fs / N_FFT as f64;
        let k0 = (f_lo / df).ceil() as usize;
        let k1 = (f_hi / df).floor() as usize;
        if k1 >= N_FFT / 2 || k1 <= k0 {
            return None;
        }
        Some(Self {
            fs,
            win,
            k0,
            nb: k1 - k0 + 1,
        })
    }

    /// Width of one FFT bin, Hz.
    pub fn df(&self) -> f64 {
        self.fs / N_FFT as f64
    }

    /// Centre frequency of band bin `k`, Hz.
    pub fn hz(&self, k: usize) -> f64 {
        (self.k0 + k) as f64 * self.df()
    }

    /// Upper edge of the band, Hz.
    pub fn top(&self) -> f64 {
        (self.k0 + self.nb) as f64 * self.df()
    }

    /// Effective number of independent bins in the band: zero padding and the Hann window
    /// (noise bandwidth 1.5 bins) correlate neighbouring bins.
    pub fn n_eff(&self) -> f64 {
        self.nb as f64 * self.win as f64 / N_FFT as f64 / 1.5
    }
}

/// The atoms of one event and their columns.
pub struct Dict {
    lobe: Vec<f32>,
    /// Normalised inner product of two columns, by distance in bins of the window length.
    overlap: Vec<f32>,
    grid: Grid,
    n: usize,
    pitch: [i32; MAX_ATOMS],
    f0: [f64; MAX_ATOMS],
    /// Fundamental at the nominal pitch (0 cents), Hz.
    nominal: [f64; MAX_ATOMS],
    /// Offset of `f0` from `nominal`, cents.
    cents: [f64; MAX_ATOMS],
    /// Partials of each atom inside the band.
    np: [u8; MAX_ATOMS],
    /// Per column (`atom · MAX_PARTIALS + partial`): first band bin, length, sum of squares.
    start: Vec<u16>,
    len: Vec<u8>,
    norm: Vec<f32>,
    /// `MAX_LOBE` values per column.
    vals: Vec<f32>,
    /// Centre of each column, Hz.
    centre: Vec<f64>,
}

impl Default for Dict {
    fn default() -> Self {
        Self::new()
    }
}

impl Dict {
    pub fn new() -> Self {
        let steps = (LOBE_HALF as usize) * LOBE_STEPS;
        let lobe: Vec<f32> = (0..=steps)
            .map(|i| hann_lobe(i as f64 / LOBE_STEPS as f64) as f32)
            .collect();
        // Overlap of two lobes d bins apart, by a Riemann sum fine enough for the table.
        let dx = 1.0 / 64.0;
        let span = (2.0 * LOBE_HALF / dx) as usize;
        let prod = |d: f64| -> f64 {
            (0..=span)
                .map(|i| {
                    let x = -LOBE_HALF + i as f64 * dx;
                    let (a, b) = (x.abs(), (x - d).abs());
                    if a > LOBE_HALF || b > LOBE_HALF {
                        0.0
                    } else {
                        hann_lobe(a) * hann_lobe(b)
                    }
                })
                .sum()
        };
        let unit = prod(0.0);
        let overlap = (0..=(2.0 * LOBE_HALF) as usize * OVERLAP_STEPS)
            .map(|i| (prod(i as f64 / OVERLAP_STEPS as f64) / unit) as f32)
            .collect();
        let cols = MAX_ATOMS * MAX_PARTIALS;
        Self {
            lobe,
            overlap,
            grid: Grid {
                fs: 24_000.0,
                win: N_FFT,
                k0: 0,
                nb: 1,
            },
            n: 0,
            pitch: [0; MAX_ATOMS],
            f0: [0.0; MAX_ATOMS],
            nominal: [0.0; MAX_ATOMS],
            cents: [0.0; MAX_ATOMS],
            np: [0; MAX_ATOMS],
            start: vec![0; cols],
            len: vec![0; cols],
            norm: vec![0.0; cols],
            vals: vec![0.0; cols * MAX_LOBE],
            centre: vec![0.0; cols],
        }
    }

    /// Forget every atom and take a new grid.
    pub fn reset(&mut self, grid: Grid) {
        self.grid = grid;
        self.n = 0;
    }

    pub fn grid(&self) -> Grid {
        self.grid
    }

    /// Number of atoms.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// MIDI note of atom `i`.
    pub fn pitch(&self, i: usize) -> i32 {
        self.pitch[i]
    }

    /// Fundamental of atom `i`, Hz.
    pub fn f0(&self, i: usize) -> f64 {
        self.f0[i]
    }

    /// Fundamental of atom `i` at its nominal pitch (no offset), Hz.
    pub fn nominal(&self, i: usize) -> f64 {
        self.nominal[i]
    }

    /// Offset of atom `i` from its nominal pitch, cents.
    pub fn cents(&self, i: usize) -> f64 {
        self.cents[i]
    }

    /// Partials of atom `i` inside the band; partial index `h` is 0-based (`h = 0` is the
    /// fundamental).
    pub fn partials(&self, i: usize) -> usize {
        self.np[i] as usize
    }

    #[inline]
    fn lobe_at(&self, x: f64) -> f32 {
        let t = x.abs() * LOBE_STEPS as f64;
        let i = t as usize;
        if i + 1 >= self.lobe.len() {
            return 0.0;
        }
        let fr = (t - i as f64) as f32;
        self.lobe[i] + (self.lobe[i + 1] - self.lobe[i]) * fr
    }

    /// Index of the atom for MIDI note `midi` (A4 = `a4` Hz), added if new. `None` when the
    /// dictionary is full or the fundamental lies outside 30 Hz to the top of the band.
    pub fn add(&mut self, midi: i32, a4: f64) -> Option<usize> {
        self.add_detuned(midi, a4, 0.0)
    }

    /// As [`Dict::add`], with the new atom `cents` off its nominal pitch. An atom already
    /// there for `midi` is returned as it is (use [`Dict::retune`] to move it).
    pub fn add_detuned(&mut self, midi: i32, a4: f64, cents: f64) -> Option<usize> {
        if let Some(i) = self.pitch[..self.n].iter().position(|&p| p == midi) {
            return Some(i);
        }
        if self.n == MAX_ATOMS || !cents.is_finite() {
            return None;
        }
        let nominal = mtof(midi as f64, a4);
        let f0 = nominal * (cents / 1200.0).exp2();
        if !f0.is_finite() || !(MIN_F0..self.grid.top()).contains(&f0) {
            return None;
        }
        let i = self.n;
        self.pitch[i] = midi;
        self.nominal[i] = nominal;
        self.build(i, f0, cents);
        self.n += 1;
        Some(i)
    }

    /// Move atom `i` to `cents` off its nominal pitch and rebuild its columns. Nothing
    /// changes when `i` is not an atom or the new fundamental would leave 30 Hz to the top
    /// of the band.
    pub fn retune(&mut self, i: usize, cents: f64) {
        if i >= self.n || !cents.is_finite() || cents == self.cents[i] {
            return;
        }
        let f0 = self.nominal[i] * (cents / 1200.0).exp2();
        if f0.is_finite() && (MIN_F0..self.grid.top()).contains(&f0) {
            self.build(i, f0, cents);
        }
    }

    /// Columns of atom `i` for fundamental `f0`.
    fn build(&mut self, i: usize, f0: f64, cents: f64) {
        let top = self.grid.top();
        let mut np = 0usize;
        while np < MAX_PARTIALS {
            let f = (np + 1) as f64 * f0;
            if f >= top {
                break;
            }
            self.column(i, np, f);
            np += 1;
        }
        self.f0[i] = f0;
        self.cents[i] = cents;
        self.np[i] = np as u8;
    }

    /// Column of partial `h` of atom `i`, centred on `hz`.
    fn column(&mut self, i: usize, h: usize, hz: f64) {
        let g = self.grid;
        // FFT bins per bin of the window length, and the lobe half-width in FFT bins.
        let scale = g.win as f64 / N_FFT as f64;
        let half = LOBE_HALF / scale;
        // Position of the partial in band bins (fractional, may be below the band).
        let p = hz / g.df() - g.k0 as f64;
        let from = (p - half).ceil().max(0.0) as usize;
        let to = ((p + half).floor().min(g.nb as f64 - 1.0)).max(-1.0) as isize;
        let c = i * MAX_PARTIALS + h;
        let row = c * MAX_LOBE;
        let n = if to >= from as isize {
            (to as usize + 1 - from).min(MAX_LOBE)
        } else {
            0
        };
        let mut sq = 0.0f32;
        for k in 0..n {
            let v = self.lobe_at(((from + k) as f64 - p) * scale);
            self.vals[row + k] = v;
            sq += v * v;
        }
        self.start[c] = from as u16;
        self.len[c] = n as u8;
        self.norm[c] = sq;
        self.centre[c] = hz;
    }

    /// Centre of the column of partial `h` of atom `i`, Hz.
    pub fn centre(&self, i: usize, h: usize) -> f64 {
        self.centre[i * MAX_PARTIALS + h]
    }

    /// One column for partials that sit closer than `tol_hz`: two columns a fraction of a
    /// bin apart cannot be told apart by any signal and leave the fit nearly singular.
    /// Partials of the atoms in `keep` that meet share the mean of their places; then a
    /// partial of any other atom moves onto the nearest column of an atom in `keep`; the
    /// partials of the other atoms that are left and meet share the mean of their places.
    pub fn tie(&mut self, keep: u64, tol_hz: f64) {
        let keep = keep & self.mask();
        let mut done = [0u64; MAX_ATOMS];
        let (kept, n) = self.by_pitch(keep);
        self.gather(&kept[..n], &mut done, tol_hz);
        // Every other atom onto the columns of `keep`.
        for i in 0..self.n {
            if keep & (1u64 << i) != 0 {
                continue;
            }
            for h in 0..self.np[i] as usize {
                let f = self.centre(i, h);
                let mut best = (tol_hz, f64::NAN);
                for &j in &kept[..n] {
                    if let Some(hj) = self.nearest(j, f) {
                        let d = (self.centre(j, hj) - f).abs();
                        if d < best.0 {
                            best = (d, self.centre(j, hj));
                        }
                    }
                }
                if best.1.is_finite() {
                    self.column(i, h, best.1);
                    done[i] |= 1u64 << h;
                }
            }
        }
        let (rest, m) = self.by_pitch(self.mask() & !keep);
        self.gather(&rest[..m], &mut done, tol_hz);
    }

    /// Atoms of `mask`, lowest pitch first, and how many.
    fn by_pitch(&self, mask: u64) -> ([usize; MAX_ATOMS], usize) {
        let mut order = [0usize; MAX_ATOMS];
        let mut n = 0usize;
        let mut bits = mask;
        while bits != 0 {
            let i = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            order[n] = i;
            n += 1;
        }
        order[..n].sort_unstable_by(|&a, &b| self.f0[a].total_cmp(&self.f0[b]));
        (order, n)
    }

    /// For each partial of `order` not yet `done`, lowest atom first: the partials of the
    /// later atoms within `tol_hz` of it join it on one column at the mean of their places.
    fn gather(&mut self, order: &[usize], done: &mut [u64; MAX_ATOMS], tol_hz: f64) {
        for (a, &i) in order.iter().enumerate() {
            for h in 0..self.np[i] as usize {
                if done[i] & (1u64 << h) != 0 {
                    continue;
                }
                let f = self.centre(i, h);
                let (mut sum, mut cnt) = (f, 1.0f64);
                let mut members = [(0usize, 0usize); MAX_ATOMS];
                let mut m = 0usize;
                for &j in &order[a + 1..] {
                    if let Some(hj) = self.nearest(j, f)
                        && done[j] & (1u64 << hj) == 0
                        && (self.centre(j, hj) - f).abs() < tol_hz
                    {
                        members[m] = (j, hj);
                        m += 1;
                        sum += self.centre(j, hj);
                        cnt += 1.0;
                    }
                }
                if m == 0 {
                    continue;
                }
                let at = sum / cnt;
                self.column(i, h, at);
                done[i] |= 1u64 << h;
                for &(j, hj) in &members[..m] {
                    self.column(j, hj, at);
                    done[j] |= 1u64 << hj;
                }
            }
        }
    }

    /// Bit mask of every atom.
    fn mask(&self) -> u64 {
        if self.n >= 64 {
            u64::MAX
        } else {
            (1u64 << self.n) - 1
        }
    }

    /// Partial of atom `j` nearest to `hz`, if it is inside the band.
    fn nearest(&self, j: usize, hz: f64) -> Option<usize> {
        let k = (hz / self.f0[j]).round();
        if k >= 1.0 && k <= f64::from(self.np[j]) {
            Some(k as usize - 1)
        } else {
            None
        }
    }

    /// Energy of `y` (a band spectrum of this grid) that one column centred on `hz` would
    /// capture on its own: `(Σ y·c)² / Σ c²`. 0 outside the band.
    pub fn capture(&self, y: &[f32], hz: f64) -> f64 {
        let g = self.grid;
        let scale = g.win as f64 / N_FFT as f64;
        let half = LOBE_HALF / scale;
        let p = hz / g.df() - g.k0 as f64;
        if !p.is_finite() || p + half < 0.0 || p - half > g.nb as f64 - 1.0 {
            return 0.0;
        }
        let from = (p - half).ceil().max(0.0) as usize;
        let to = ((p + half)
            .floor()
            .min(g.nb as f64 - 1.0)
            .min(y.len() as f64 - 1.0)) as usize;
        let (mut dot, mut sq) = (0.0f64, 0.0f64);
        for (k, yv) in y.iter().enumerate().take(to + 1).skip(from) {
            let v = f64::from(self.lobe_at((k as f64 - p) * scale));
            dot += f64::from(*yv) * v;
            sq += v * v;
        }
        if sq > 0.0 { dot * dot / sq } else { 0.0 }
    }

    /// Column of partial `h` of atom `i`: first band bin and values.
    #[inline]
    pub fn lobe(&self, i: usize, h: usize) -> (usize, &[f32]) {
        let c = i * MAX_PARTIALS + h;
        let row = c * MAX_LOBE;
        (
            self.start[c] as usize,
            &self.vals[row..row + self.len[c] as usize],
        )
    }

    /// Sum of squares of that column.
    #[inline]
    pub fn norm(&self, i: usize, h: usize) -> f64 {
        f64::from(self.norm[i * MAX_PARTIALS + h])
    }

    /// Normalised inner product of two columns `hz` apart: 1 for the same place, 0 beyond
    /// eight bins of the window length.
    pub fn overlap(&self, hz: f64) -> f32 {
        let d = hz.abs() * self.grid.win as f64 / self.grid.fs;
        let t = d * OVERLAP_STEPS as f64;
        let i = t as usize;
        if i + 1 >= self.overlap.len() {
            return 0.0;
        }
        let fr = (t - i as f64) as f32;
        self.overlap[i] + (self.overlap[i + 1] - self.overlap[i]) * fr
    }

    /// How much of partial `h` of atom `i` no atom in `others` (a bit mask of atoms) can
    /// stand in for: 1 when it stands alone, 0 when another atom has a partial in the same
    /// place.
    pub fn alone(&self, i: usize, h: usize, others: u64) -> f32 {
        let f = (h + 1) as f64 * self.f0[i];
        let mut worst = 0.0f32;
        let mut bits = others & !(1u64 << i);
        while bits != 0 {
            let j = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            if j >= self.n {
                break;
            }
            // Nearest partial of atom j; its comb goes on above the band, so no upper cap.
            let hj = (f / self.f0[j]).round().max(1.0);
            worst = worst.max(self.overlap(f - hj * self.f0[j]));
        }
        1.0 - worst
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(win: usize) -> Grid {
        Grid::new(24_000.0, win, 70.0, 5000.0).unwrap()
    }

    #[test]
    fn lobe_is_continuous_and_normalised() {
        assert_eq!(hann_lobe(0.0), 1.0);
        assert!((hann_lobe(1.0) - 0.5).abs() < 1e-12);
        assert!((hann_lobe(1.0 + 1e-5) - 0.5).abs() < 1e-4);
        assert!((hann_lobe(0.5) - 0.848_826_363).abs() < 1e-6);
        for z in [2.0, 3.0, 4.0] {
            assert!(hann_lobe(z) < 1e-9, "zero at {z}");
        }
        // first side lobe of the Hann window: -31.5 dB
        let side = (200..300)
            .map(|i| hann_lobe(i as f64 / 100.0))
            .fold(0.0, f64::max);
        assert!((20.0 * side.log10() + 31.5).abs() < 0.2, "{side}");
    }

    #[test]
    fn grid_band_and_rejections() {
        let g = grid(N_FFT);
        assert!(g.hz(0) >= 70.0 && g.hz(0) < 70.0 + g.df());
        assert!(g.hz(g.nb - 1) <= 5000.0 && g.hz(g.nb - 1) > 5000.0 - g.df());
        assert!((g.n_eff() - g.nb as f64 / 1.5).abs() < 1e-9);
        assert!(Grid::new(9000.0, N_FFT, 70.0, 5000.0).is_none());
        assert!(Grid::new(24_000.0, 0, 70.0, 5000.0).is_none());
        assert!(Grid::new(24_000.0, N_FFT + 1, 70.0, 5000.0).is_none());
        assert!(Grid::new(f64::NAN, N_FFT, 70.0, 5000.0).is_none());
        // 36 ms at 24 kHz: a column would need more than MAX_LOBE bins
        assert!(Grid::new(24_000.0, 860, 70.0, 5000.0).is_none());
        assert!(Grid::new(24_000.0, 1440, 70.0, 5000.0).is_some());
    }

    #[test]
    fn columns_sit_on_the_partials() {
        let mut d = Dict::new();
        d.reset(grid(N_FFT));
        let i = d.add(57, 440.0).unwrap(); // A3, 220 Hz
        assert_eq!(d.add(57, 440.0), Some(i), "same pitch, same atom");
        assert_eq!(d.partials(i), 22, "220 Hz to 4840 Hz");
        let g = d.grid();
        for h in [0usize, 3, 21] {
            let (start, vals) = d.lobe(i, h);
            let peak = vals
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap();
            let hz = g.hz(start + peak.0);
            let want = (h + 1) as f64 * 220.0;
            assert!((hz - want).abs() <= g.df() / 2.0 + 1e-9, "h {h}: {hz}");
            assert!(*peak.1 > 0.84 && *peak.1 <= 1.0);
            assert_eq!(vals.len(), 8, "main lobe and first side lobes");
            let sq: f64 = vals.iter().map(|v| (*v as f64).powi(2)).sum();
            assert!((d.norm(i, h) - sq).abs() < 1e-6);
        }
    }

    #[test]
    fn overlap_and_alone() {
        let mut d = Dict::new();
        d.reset(grid(N_FFT));
        let df = d.grid().df();
        assert!((d.overlap(0.0) - 1.0).abs() < 1e-6);
        assert!(
            d.overlap(df) > 0.5 && d.overlap(df) < 0.9,
            "{}",
            d.overlap(df)
        );
        assert!(d.overlap(3.0 * df) < 0.05);
        assert_eq!(d.overlap(9.0 * df), 0.0);
        let e3 = d.add(52, 440.0).unwrap();
        let e4 = d.add(64, 440.0).unwrap();
        let fs4 = d.add(66, 440.0).unwrap();
        let all = 0b111u64;
        // every partial of E4 is a partial of E3: an octave-doubled string has nothing alone
        for h in 0..d.partials(e4) {
            assert!(d.alone(e4, h, all) < 1e-3, "E4 partial {h}");
        }
        // E3 keeps its odd partials, F#4 its first ones
        assert!(d.alone(e3, 0, all) > 0.999 && d.alone(e3, 2, all) > 0.999);
        assert!(d.alone(e3, 1, all) < 1e-3);
        assert!(d.alone(fs4, 0, all) > 0.999 && d.alone(fs4, 1, all) > 0.999);
        // an atom is never compared with itself
        assert_eq!(d.alone(e4, 0, 1u64 << e4), 1.0);
    }

    #[test]
    fn short_window_widens_the_lobe() {
        let mut d = Dict::new();
        d.reset(grid(N_FFT));
        let i = d.add(69, 440.0).unwrap();
        let narrow = d.lobe(i, 0).1.len();
        let near = d.overlap(12.0);
        d.reset(grid(N_FFT / 2));
        let j = d.add(69, 440.0).unwrap();
        assert!(d.lobe(j, 0).1.len() >= 2 * narrow - 1);
        assert!(
            d.overlap(12.0) > near + 0.3,
            "12 Hz apart is no longer resolved"
        );
    }

    #[test]
    fn refuses_out_of_band_and_overflow() {
        let mut d = Dict::new();
        d.reset(grid(N_FFT));
        assert!(d.add(0, 440.0).is_none(), "8 Hz");
        assert!(d.add(127, 440.0).is_none(), "12.5 kHz");
        for m in 40..40 + MAX_ATOMS as i32 {
            assert!(d.add(m, 440.0).is_some());
        }
        assert_eq!(d.len(), MAX_ATOMS);
        assert!(d.add(39, 440.0).is_none(), "full");
        assert!(
            d.add(45, 440.0).is_some(),
            "an existing atom is still found"
        );
        // E2 has 60 partials below 5 kHz; a 31 Hz note is cut at MAX_PARTIALS
        assert_eq!(d.partials(0), 60);
        d.reset(grid(N_FFT));
        let low = d.add(23, 440.0).unwrap();
        assert_eq!(d.partials(low), MAX_PARTIALS);
    }
}
