//! DSP witness of the chord verifier (the verifier/learner design plan section 2.4, second part).
//!
//! [`CombWitness`] answers one question about one strum: of the hypotheses in the closed
//! [`ConfusionSet`], which one explains the energy that is **new** after the stroke?
//!
//! 1. `pre` and `post` are decimated to about 24 kHz, Hann-windowed and transformed
//!    ([`N_FFT`] points); the magnitudes from `f_lo` to `f_hi` are kept, in amplitude units.
//! 2. New energy: `y = max(0, post - ring_gain · pre)`, bin by bin. Whatever was already
//!    sounding (a ringing chord, hum, the noise floor) can only stay or decay, so it is
//!    removed before the fit.
//! 3. A hypothesis is a set of atoms ([`crate::atoms`]): harmonic combs with one free,
//!    non-negative gain per partial. Coordinate descent on the residual gives its `RSS`.
//!    Two hypotheses are always fitted as a pair: an atom one of them lacks may not use
//!    its partials that sit (within `min_alone`) on a partial of an atom of the other.
//!    There the two cannot differ, and the slight misfit of a real partial (decay,
//!    beating of two strings a few hertz apart) would otherwise pass for evidence.
//! 4. `g(a, b) = N_ef · ln((RSS_a + floor) / (RSS_b + floor))`, with
//!    `floor = model_floor · Σy²`: no model is trusted below that share of the new energy,
//!    so a trace at the right place is not evidence. Per string: `g_exp` (expected
//!    hypothesis without the string's atom, against with it), `g_alt` (the best
//!    alternative atom of that string added to the expected hypothesis), `obs` (share of
//!    the atom's `1/h²` energy at partials no other expected atom has) and `clear` (the
//!    part of `obs` above `obs_lo_hz` that `pre` does not cover).
//! 5. Every alternative gets a penalised log-likelihood ratio against the expected
//!    hypothesis, in nats: `Λ = g(expected, alt) / 2 + λ · (absent - added)` atoms. An atom
//!    has to earn `λ` nats to count as sounding, an absent atom is credited only when its
//!    string is `clear`, and an added atom with no partial of its own next to the other
//!    atoms of the alternative (an octave above one of them) costs nothing.
//!
//! Opinion:
//! - `Names` when the best alternative reaches `name_margin`, explains `min_explained`
//!   of the new energy, and the new energy is at least `name_snr_db` above what was
//!   already sounding (below it the answer is `LowSnr`). `alts` is a bit mask over `set.alts` (bit `i` = `set.alts[i]`)
//!   with the winner and every alternative within `tie_margin` of it. A string that is
//!   not `clear` is never called missing, and `AllMuted` is never named: a muted stroke
//!   leaves no new sustained energy, which these two windows cannot tell from a stroke
//!   that was not heard.
//! - `Supports` when every `Must` string that can be seen (`obs ≥ obs_min`) earned its
//!   place by `support_margin`, no alternative atom adds explanation (the veto), no
//!   alternative comes within `support_margin` of the expected hypothesis, and the expected
//!   atoms explain `min_explained` of the new energy. `Must` strings that cannot be seen
//!   (an octave-doubled string has no partial of its own) are left out: `Supports` says
//!   nothing about them. The margin is the smallest lead, so it never exceeds
//!   `atom_penalty`.
//! - `Abstains` otherwise: `TooShort`, `Unavailable` (bad rate, non-finite or clipped
//!   input, unusable set), `LowSnr` (silence, or new energy below what was already there),
//!   `Unobservable` (no `Must` string can be seen, or the only thing in the way is a
//!   string whose absence cannot be established) or `Ambiguous`.
//!
//! Deviations from the plan text, all measured on the development seeds before they were
//! made:
//! - "one column for what was already sounding": a single fitted column cannot follow a
//!   previous chord of which only some strings keep ringing, so the ringing sound is a
//!   per-bin floor instead of a column;
//! - "two columns per atom": see [`crate::atoms`];
//! - the pairwise fit of step 3 and the `name_snr_db` gate are additions: without them
//!   4.5% of correct strums on the development grid were named as an error (a ringing
//!   string that stops, or a near-coincident partial, read as a new string).
//!
//! No allocation after [`CombWitness::new`]; no panic on any input.

use std::sync::Arc;

use guitarness_dsp::resample::Decimator2;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::atoms::{Dict, Grid, MAX_ATOMS, MAX_BINS, MAX_PARTIALS, N_FFT};
use crate::types::{Abstain, Ask, ConfusionSet, Family, HypKind, Opinion, Window, Witness};

/// Alternatives one event can hold: the width of the `Names` mask.
pub const MAX_ALTS: usize = 32;
/// `pre` ends this long before the attack, ms.
pub const PRE_GUARD_MS: f64 = 5.0;
/// `post` starts this long after the last string of the stroke, ms.
pub const POST_GUARD_MS: f64 = 10.0;
/// Seeds of the synthetic strums used to choose [`ExciteConfig::default`]. A held-out
/// bench must draw its seeds outside this range.
pub const DEV_SEEDS: std::ops::Range<u32> = 0..10_000;

/// Sample rates the witness accepts, Hz.
const MIN_FS: f64 = 11_000.0;
const MAX_FS: f64 = 200_000.0;
const MEMO: usize = 192;
/// Coordinate descent stops when one sweep lowers the residual by less than this share of
/// the new energy (far below any `model_floor`), or after `MAX_SWEEPS`.
const CD_TOL: f64 = 1e-9;
const MAX_SWEEPS: u32 = 2000;
/// Partials that set the level of a string: the first few carry most of its energy.
const LEVEL_PARTIALS: usize = 4;
const NO_ATOM: u8 = u8::MAX;
/// Tuning fit: partials per atom it looks at, and the step of the guitar-wide scan, cents.
const TUNE_PARTIALS: usize = 16;
const TUNE_STEP: f64 = 5.0;
/// Partials of different atoms closer than this share one column, bins of the window
/// length (see [`Dict::tie`]).
const TIE_BINS: f64 = 0.5;
/// The guitar-wide offset moves to the median of the atoms' own offsets when that is
/// further than this, cents.
const RECENTRE_CENTS: f64 = 0.5;
/// Points of the guitar-wide scan the stack scratch holds (±80 cents at 5-cent steps).
const MAX_TUNE_STEPS: usize = 33;
/// A second peak of the guitar-wide scan this close to the best one, and further than
/// `RIVAL_CENTS` from it, makes the offset ambiguous.
const RIVAL_SHARE: f64 = 0.8;
const RIVAL_CENTS: f64 = 10.0;
/// Partials of an alternative atom this close to a partial of another atom are not
/// evidence: a string may sit this far off its own pitch.
const GUARD_CENTS: f64 = 20.0;
/// An alternative atom without an expected atom of its pitch class is placed only when its
/// partials agree: at least `AGREE_SHARE` of the weight within `AGREE_CENTS` of the
/// median, from at least `AGREE_MIN_PARTIALS` partials.
const AGREE_CENTS: f64 = 3.0;
const AGREE_SHARE: f64 = 0.6;
const AGREE_MIN_PARTIALS: usize = 2;

/// Operating point of the witness. The defaults were chosen on [`DEV_SEEDS`] and are frozen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExciteConfig {
    /// Band of the fit, Hz.
    pub f_lo: f64,
    pub f_hi: f64,
    /// Reference pitch, Hz.
    pub a4: f64,
    /// Semitones added to every atom. 0 always, except in the negative control.
    pub dict_shift: i32,
    /// Where the guitar is expected to be against `a4` (a tuner reading), cents.
    pub ref_cents: f32,
    /// The guitar-wide offset is searched this far either side of `ref_cents`, cents. An
    /// offset found at the last step of that range is reported as [`Fault::Detuned`].
    pub tune_range_cents: f32,
    /// Each atom may move this far from the guitar-wide offset, cents.
    pub string_range_cents: f32,
    /// Least ratio of the peak of the guitar-wide scan to its median for the offset to be
    /// trusted; below it `Names` is withheld.
    pub tune_sharp_min: f32,
    /// Shortest usable `post` and `pre`, ms.
    pub min_post_ms: f64,
    pub min_pre_ms: f64,
    /// `pre` times this is the floor below which nothing counts as new.
    pub ring_gain: f32,
    /// Share of the new energy below which no residual is trusted.
    pub model_floor: f32,
    /// λ: nats an atom has to earn to count as sounding.
    pub atom_penalty: f32,
    /// Lead of the expected hypothesis needed for `Supports`, nats.
    pub support_margin: f32,
    /// Lead of an alternative over the expected hypothesis needed for `Names`, nats.
    pub name_margin: f32,
    /// Alternatives this close to the winner share the `Names` mask, nats.
    pub tie_margin: f32,
    /// Least `alone` (one minus the overlap with the nearest partial of an expected atom)
    /// a partial of any other atom needs to take part in a fit.
    pub min_alone: f32,
    /// O_min: least `obs` at which a `Must` string has to show, and least `clear` at which
    /// a string may be called absent.
    pub obs_min: f32,
    /// Partials below this frequency never prove an absence (unknown microphone), Hz.
    pub obs_lo_hz: f64,
    /// A partial is covered when `pre` reaches this share of the level it would have.
    pub mask_ratio: f32,
    /// Below this ratio of new energy to `pre` energy an abstention is reported as `LowSnr`, dB.
    pub snr_min_db: f32,
    /// `Names` needs at least this ratio of new energy to `pre` energy, dB: where the new
    /// sound is weaker than what was ringing, a partial that stops (a string struck again,
    /// two strings beating) leaves magnitude differences that look like new partials.
    pub name_snr_db: f32,
    /// Least share of the new energy the winning hypothesis has to explain.
    pub min_explained: f32,
    /// `post` below this RMS is silence, dBFS.
    pub min_level_dbfs: f32,
    /// Samples at or above this magnitude count as clipped; above `clip_frac` of the
    /// window the input is refused.
    pub clip_level: f32,
    pub clip_frac: f32,
}

impl Default for ExciteConfig {
    fn default() -> Self {
        Self {
            f_lo: 70.0,
            f_hi: 5000.0,
            a4: 440.0,
            dict_shift: 0,
            ref_cents: 0.0,
            tune_range_cents: 40.0,
            string_range_cents: 12.0,
            tune_sharp_min: 1.5,
            min_post_ms: 60.0,
            min_pre_ms: 20.0,
            ring_gain: 1.0,
            model_floor: 0.01,
            atom_penalty: 20.0,
            support_margin: 10.0,
            name_margin: 15.0,
            tie_margin: 5.0,
            min_alone: 0.6,
            obs_min: 0.3,
            obs_lo_hz: 150.0,
            mask_ratio: 0.25,
            snr_min_db: -3.0,
            name_snr_db: 0.0,
            min_explained: 0.6,
            min_level_dbfs: -70.0,
            clip_level: 0.99,
            clip_frac: 0.002,
        }
    }
}

/// Why the input could not be analysed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Sample rate not finite or outside 11 kHz to 200 kHz.
    BadRate,
    /// A NaN or an infinity in a window.
    NonFinite,
    Clipped,
    PostShort,
    PreShort,
    Silent,
    /// The set names more than [`MAX_ALTS`] alternatives, a pitch outside the band, more
    /// pitches than the dictionary holds, or nothing at all to sound.
    BadSet,
    /// The guitar is further from the reference pitch than the tuning fit searches: tune
    /// it (or give the witness a new reference).
    Detuned,
}

impl Fault {
    pub fn abstain(self) -> Abstain {
        match self {
            Fault::BadRate | Fault::NonFinite | Fault::Clipped | Fault::BadSet | Fault::Detuned => {
                Abstain::Unavailable
            }
            Fault::PostShort | Fault::PreShort => Abstain::TooShort,
            Fault::Silent => Abstain::LowSnr,
        }
    }
}

/// Statistics of one string for one event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StringStat {
    /// `g(expected without this string's atom, expected)`; 0 when nothing is expected on
    /// the string or another expected string plays the same note.
    pub g_exp: f32,
    /// `g(expected, expected plus the atom)` for the best alternative atom of this string;
    /// 0 without one.
    pub g_alt: f32,
    /// Share of the expected atom's `1/h²` energy at partials no other expected atom has.
    pub obs: f32,
    /// The part of `obs` that could prove an absence: above `obs_lo_hz` and not covered by
    /// what was already sounding.
    pub clear: f32,
}

/// Everything the witness worked out for the last event.
#[derive(Clone, Copy, Debug)]
pub struct Analysis {
    pub opinion: Opinion,
    pub fault: Option<Fault>,
    pub strings: [Option<StringStat>; 6],
    /// Alternatives scored (`set.alts.len()`).
    pub n_alts: usize,
    /// Penalised log-likelihood ratio Λ of each alternative against the expected
    /// hypothesis, nats: what `Names` ranks.
    pub score: [f32; MAX_ALTS],
    /// Best `g / 2 - λ` over the alternative atoms added one at a time, nats.
    pub veto: f32,
    /// What stands in the way of `Supports`, nats: `Supports` needs `-block ≥ support_margin`.
    pub block: f32,
    /// New energy over `pre` energy, dB.
    pub snr_db: f32,
    /// Share of the new energy the expected hypothesis explains.
    pub explained: f32,
    pub n_eff: f32,
    /// Coordinate-descent sweeps spent on this event.
    pub sweeps: u32,
    /// A fit stopped at the sweep limit. The opinion is then `Abstains(Ambiguous)`.
    pub capped: bool,
    /// Offset of the guitar from `a4` found for this strum, cents.
    pub offset_cents: f32,
    /// Largest move of an expected atom away from `offset_cents`, cents (signed).
    pub atom_cents_max: f32,
    /// Peak over median of the guitar-wide scan.
    pub tune_sharp: f32,
    /// The offset is clear (sharp peak, no rival) and no expected atom hit the edge of its
    /// range. Without it `Names` becomes `Abstains(Ambiguous)`.
    pub tune_ok: bool,
}

impl Analysis {
    fn empty() -> Self {
        Self {
            opinion: Opinion::Abstains(Abstain::Unavailable),
            fault: None,
            strings: [None; 6],
            n_alts: 0,
            score: [f32::NAN; MAX_ALTS],
            veto: f32::NAN,
            block: f32::NAN,
            snr_db: f32::NAN,
            explained: f32::NAN,
            n_eff: 0.0,
            sweeps: 0,
            capped: false,
            offset_cents: f32::NAN,
            atom_cents_max: f32::NAN,
            tune_sharp: f32::NAN,
            tune_ok: false,
        }
    }
}

/// Number of half-band decimations for a sample rate; `None` if the rate is unusable.
fn stages(fs: f64) -> Option<u32> {
    if !fs.is_finite() || !(MIN_FS..=MAX_FS).contains(&fs) {
        return None;
    }
    Some(if fs < 32_000.0 {
        0
    } else if fs < 64_000.0 {
        1
    } else {
        2
    })
}

/// Longest stretch of input one window can use at this rate, samples (0 for a bad rate).
pub fn frame_len(fs: f64) -> usize {
    stages(fs).map_or(0, |st| N_FFT << st)
}

/// Cut the `pre` and `post` windows of one strum out of a recording, by the convention of
/// plan section 2.4. `attack` is the first string and `last_string` the last one, in
/// samples. `prev_end` is where the stroke before ended (0 if none) and `next_attack` where
/// the next one starts (`audio.len()` if none): neither window crosses them. A window that
/// does not fit comes back short or empty, and the witness answers `TooShort`.
pub fn cut(
    audio: &[f32],
    fs: f64,
    attack: usize,
    last_string: usize,
    prev_end: usize,
    next_attack: usize,
) -> Window<'_> {
    let frame = frame_len(fs);
    let ms = |t: f64| {
        if fs.is_finite() && fs > 0.0 {
            (t * 1e-3 * fs).round() as usize
        } else {
            0
        }
    };
    let n = audio.len();
    let pre_end = attack.saturating_sub(ms(PRE_GUARD_MS)).min(n);
    let pre_start = pre_end.saturating_sub(frame).max(prev_end).min(pre_end);
    let post_start = last_string
        .max(attack)
        .saturating_add(ms(POST_GUARD_MS))
        .min(n);
    let post_end = post_start
        .saturating_add(frame)
        .min(next_attack.saturating_sub(ms(PRE_GUARD_MS)))
        .min(n)
        .max(post_start);
    Window {
        fs,
        pre: &audio[pre_start..pre_end],
        post: &audio[post_start..post_end],
    }
}

/// Sum of squares, count of clipped samples, and whether every sample is finite.
fn scan(x: &[f32], clip_level: f32) -> (f64, usize, bool) {
    let mut sq = 0.0f64;
    let mut clipped = 0usize;
    let mut finite = true;
    for &v in x {
        finite &= v.is_finite();
        sq += v as f64 * v as f64;
        clipped += usize::from(v.abs() >= clip_level);
    }
    (sq, clipped, finite)
}

struct Fft {
    plan: Arc<dyn RealToComplex<f32>>,
    hann: Vec<f32>,
    fin: Vec<f32>,
    fout: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Fft {
    fn new() -> Self {
        let plan = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
        let hann = (0..N_FFT)
            .map(|n| {
                (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / N_FFT as f64).cos()) as f32
            })
            .collect();
        Self {
            fin: plan.make_input_vec(),
            fout: plan.make_output_vec(),
            scratch: plan.make_scratch_vec(),
            hann,
            plan,
        }
    }

    /// Magnitude spectrum of `x` (exactly `win << st` samples) over the band of `grid`,
    /// in amplitude units: a steady sine of amplitude 1 peaks at 1. `false` if the
    /// transform refuses the buffers (it cannot: they come from the plan).
    fn spectrum(&mut self, x: &[f32], st: u32, win: usize, grid: &Grid, out: &mut [f32]) -> bool {
        let (mut d1, mut d2) = (Decimator2::new(), Decimator2::new());
        let mut n = 0usize;
        for &v in x {
            let u = match st {
                0 => Some(v),
                1 => d1.push(v),
                _ => d1.push(v).and_then(|u| d2.push(u)),
            };
            if let Some(u) = u
                && n < win
            {
                self.fin[n] = u;
                n += 1;
            }
        }
        if n == N_FFT {
            for (v, w) in self.fin.iter_mut().zip(&self.hann) {
                *v *= *w;
            }
        } else {
            let step = 2.0 * std::f64::consts::PI / n.max(1) as f64;
            for (i, v) in self.fin[..n].iter_mut().enumerate() {
                *v *= (0.5 - 0.5 * (step * i as f64).cos()) as f32;
            }
            self.fin[n..].fill(0.0);
        }
        if self
            .plan
            .process_with_scratch(&mut self.fin, &mut self.fout, &mut self.scratch)
            .is_err()
        {
            return false;
        }
        // Coherent gain of a Hann window of n samples is n / 2.
        let norm = 4.0 / n.max(1) as f32;
        for (o, c) in out[..grid.nb].iter_mut().zip(&self.fout[grid.k0..]) {
            *o = c.norm() * norm;
        }
        true
    }
}

/// The DSP witness: FFT plan, dictionary and scratch, all allocated by [`CombWitness::new`].
pub struct CombWitness {
    cfg: ExciteConfig,
    fft: Fft,
    dict: Dict,
    post: Vec<f32>,
    pre: Vec<f32>,
    /// New energy: `max(0, post - ring_gain · pre)`.
    y: Vec<f32>,
    /// Residual of the fit in progress, and of the expected hypothesis.
    resid: Vec<f64>,
    resid_exp: Vec<f64>,
    /// Partial gains (`atom · MAX_PARTIALS + partial`) of the fit in progress, and of the
    /// expected hypothesis: every other fit starts from the latter.
    gain: Vec<f64>,
    gain_exp: Vec<f64>,
    exp_mask: u64,
    /// `coll[i · MAX_ATOMS + j]`: partials (bit `h`) of atom `i` that sit on a partial of
    /// atom `j`.
    coll: Vec<u64>,
    /// Partials each atom may not use in the fit in progress.
    skip: [u64; MAX_ATOMS],
    have_exp: bool,
    memo: [(u64, u64, f64); MEMO],
    memo_n: usize,
    /// Sum of `y²`: the residual of the empty hypothesis.
    e_new: f64,
    last: Analysis,
}

impl Default for CombWitness {
    fn default() -> Self {
        Self::new(ExciteConfig::default())
    }
}

/// What the partials of one atom say about its own offset.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Reading {
    /// Offset, cents, and the weight of the readings behind it.
    Read(f64, f64),
    /// The atom sounds off by more than the range (the offset is that edge), or its
    /// partials disagree (the offset is 0).
    Unsure(f64),
    /// No partial could be read.
    Nothing,
}

/// Weighted median of `(value, weight)` pairs; 0 when the weights sum to nothing.
fn weighted_median(items: impl Iterator<Item = (f64, f64)>) -> f64 {
    let mut v = [(0.0f64, 0.0f64); MAX_ATOMS];
    let mut n = 0usize;
    let mut total = 0.0f64;
    for (x, w) in items {
        if n < MAX_ATOMS && w > 0.0 && x.is_finite() {
            v[n] = (x, w);
            total += w;
            n += 1;
        }
    }
    if total <= 0.0 {
        return 0.0;
    }
    let v = &mut v[..n];
    v.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut acc = 0.0f64;
    for &(x, w) in v.iter() {
        acc += w;
        if acc >= 0.5 * total {
            return x;
        }
    }
    v[n - 1].0
}

/// Atoms of a mask, lowest first.
fn atoms(mut mask: u64) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        if mask == 0 {
            return None;
        }
        let i = mask.trailing_zeros() as usize;
        mask &= mask - 1;
        Some(i)
    })
}

impl CombWitness {
    pub fn new(cfg: ExciteConfig) -> Self {
        Self {
            cfg,
            fft: Fft::new(),
            dict: Dict::new(),
            post: vec![0.0; MAX_BINS],
            pre: vec![0.0; MAX_BINS],
            y: vec![0.0; MAX_BINS],
            resid: vec![0.0; MAX_BINS],
            resid_exp: vec![0.0; MAX_BINS],
            gain: vec![0.0; MAX_ATOMS * MAX_PARTIALS],
            gain_exp: vec![0.0; MAX_ATOMS * MAX_PARTIALS],
            exp_mask: 0,
            coll: vec![0; MAX_ATOMS * MAX_ATOMS],
            skip: [0; MAX_ATOMS],
            have_exp: false,
            memo: [(0, 0, 0.0); MEMO],
            memo_n: 0,
            e_new: 0.0,
            last: Analysis::empty(),
        }
    }

    pub fn config(&self) -> &ExciteConfig {
        &self.cfg
    }

    /// Where the guitar is expected to be against `a4`, cents (for instance the last tuner
    /// reading). The tuning fit searches around it.
    pub fn set_reference(&mut self, cents: f32) {
        if cents.is_finite() {
            self.cfg.ref_cents = cents;
        }
    }

    /// Analyse one event. Deterministic: the result depends on the arguments only.
    pub fn analyze(&mut self, w: &Window, set: &ConfusionSet) -> &Analysis {
        self.last = Analysis::empty();
        self.last.opinion = match self.run(w, set) {
            Ok(op) => op,
            Err(fault) => {
                self.last.fault = Some(fault);
                Opinion::Abstains(fault.abstain())
            }
        };
        &self.last
    }

    /// Per-string statistics of one event (`None`: the set says nothing about the string).
    pub fn stats(&mut self, w: &Window, set: &ConfusionSet) -> [Option<StringStat>; 6] {
        self.analyze(w, set).strings
    }

    /// The analysis behind the last opinion.
    pub fn last(&self) -> &Analysis {
        &self.last
    }

    /// Residual of the hypothesis made of the atoms in `mask` (bit `i` = atom `i`) when it
    /// is compared with the hypothesis `against`: non-negative least squares, one gain per
    /// partial, by coordinate descent on the residual. An atom of `mask` that `against` does
    /// not hold may not use its partials that sit on a partial of an atom of `against`:
    /// there the two hypotheses cannot differ, and any misfit of the other atom would pass
    /// for evidence. The first call of an event must be the expected hypothesis.
    fn fit(&mut self, mask: u64, against: u64) -> f64 {
        if mask == 0 {
            return self.e_new;
        }
        // Only the atoms `against` lacks are restricted, by the atoms it has.
        let against = if mask & !against == 0 { 0 } else { against };
        if let Some(&(_, _, v)) = self.memo[..self.memo_n]
            .iter()
            .find(|(m, o, _)| *m == mask && *o == against)
        {
            return v;
        }
        for i in atoms(mask) {
            self.skip[i] = if against & (1u64 << i) != 0 {
                0
            } else {
                atoms(against & !(1u64 << i)).fold(0, |acc, j| acc | self.coll[i * MAX_ATOMS + j])
            };
        }
        let nb = self.dict.grid().nb;
        if self.have_exp {
            // Start from the expected fit: keep the atoms and partials in common, give back
            // what the others explained.
            self.resid[..nb].copy_from_slice(&self.resid_exp[..nb]);
            for i in atoms(self.exp_mask) {
                let gone = if mask & (1u64 << i) == 0 {
                    u64::MAX
                } else {
                    self.skip[i]
                };
                if gone == 0 {
                    continue;
                }
                for h in 0..self.dict.partials(i) {
                    let a = self.gain_exp[i * MAX_PARTIALS + h];
                    if gone & (1u64 << h) != 0 && a > 0.0 {
                        let (start, vals) = self.dict.lobe(i, h);
                        for (r, v) in self.resid[start..].iter_mut().zip(vals) {
                            *r += a * f64::from(*v);
                        }
                    }
                }
            }
            for i in atoms(mask) {
                let row = i * MAX_PARTIALS..i * MAX_PARTIALS + self.dict.partials(i);
                if self.exp_mask & (1u64 << i) != 0 {
                    self.gain[row.clone()].copy_from_slice(&self.gain_exp[row.clone()]);
                    let skip = self.skip[i];
                    for (h, g) in self.gain[row].iter_mut().enumerate() {
                        if skip & (1u64 << h) != 0 {
                            *g = 0.0;
                        }
                    }
                } else {
                    self.gain[row].fill(0.0);
                }
            }
        } else {
            for (r, v) in self.resid[..nb].iter_mut().zip(&self.y[..nb]) {
                *r = f64::from(*v);
            }
            for i in atoms(mask) {
                self.gain[i * MAX_PARTIALS..i * MAX_PARTIALS + self.dict.partials(i)].fill(0.0);
            }
        }
        let tol = CD_TOL * self.e_new;
        let mut sweeps = 0u32;
        loop {
            let mut dec = 0.0f64;
            for i in atoms(mask) {
                let skip = self.skip[i];
                for h in 0..self.dict.partials(i) {
                    if skip & (1u64 << h) != 0 {
                        continue;
                    }
                    let norm = self.dict.norm(i, h);
                    if norm <= 1e-12 {
                        continue;
                    }
                    let (start, vals) = self.dict.lobe(i, h);
                    let r = &mut self.resid[start..start + vals.len()];
                    let mut dot = 0.0f64;
                    for (rv, v) in r.iter().zip(vals) {
                        dot += *rv * f64::from(*v);
                    }
                    let a = &mut self.gain[i * MAX_PARTIALS + h];
                    let next = (*a + dot / norm).max(0.0);
                    let d = next - *a;
                    if d != 0.0 {
                        for (rv, v) in r.iter_mut().zip(vals) {
                            *rv -= d * f64::from(*v);
                        }
                        *a = next;
                        dec += norm * d * d;
                    }
                }
            }
            sweeps += 1;
            if dec <= tol {
                break;
            }
            if sweeps >= MAX_SWEEPS {
                self.last.capped = true;
                break;
            }
        }
        self.last.sweeps += sweeps;
        let rss: f64 = self.resid[..nb].iter().map(|r| r * r).sum();
        if !self.have_exp {
            self.resid_exp[..nb].copy_from_slice(&self.resid[..nb]);
            for i in atoms(mask) {
                let row = i * MAX_PARTIALS..i * MAX_PARTIALS + self.dict.partials(i);
                self.gain_exp[row.clone()].copy_from_slice(&self.gain[row]);
            }
            self.exp_mask = mask;
            self.have_exp = true;
        }
        if self.memo_n < MEMO {
            self.memo[self.memo_n] = (mask, against, rss);
            self.memo_n += 1;
        }
        rss
    }

    /// Atom of string `s` at fret `fret`, added to the dictionary if new.
    fn atom(&mut self, set: &ConfusionSet, s: usize, fret: i8) -> Result<u8, Fault> {
        if !(0..=36).contains(&fret) {
            return Err(Fault::BadSet);
        }
        let midi = set.tuning[s]
            .saturating_add(i32::from(set.capo))
            .saturating_add(i32::from(fret))
            .saturating_add(self.cfg.dict_shift);
        self.dict
            .add(midi, self.cfg.a4)
            .map(|i| i as u8)
            .ok_or(Fault::BadSet)
    }

    /// Atoms of a `sounding` pattern, as a mask and per string.
    fn atoms_of(
        &mut self,
        set: &ConfusionSet,
        sounding: &[i8; 6],
    ) -> Result<(u64, [u8; 6]), Fault> {
        let mut mask = 0u64;
        let mut per = [NO_ATOM; 6];
        for s in 0..6 {
            if sounding[s] >= 0 {
                let i = self.atom(set, s, sounding[s])?;
                mask |= 1u64 << i;
                per[s] = i;
            } else if sounding[s] != -1 {
                return Err(Fault::BadSet);
            }
        }
        Ok((mask, per))
    }

    /// Level of an expected atom: its strongest low partial in the expected fit.
    fn level(&self, i: usize) -> f64 {
        let n = self.dict.partials(i).min(LEVEL_PARTIALS);
        self.gain_exp[i * MAX_PARTIALS..i * MAX_PARTIALS + n]
            .iter()
            .copied()
            .fold(0.0, f64::max)
    }

    /// Share of the `1/h²` energy of atom `i` at partials no atom of `others` has.
    fn share_alone(&self, i: usize, others: u64) -> f64 {
        let (mut total, mut alone) = (0.0f64, 0.0f64);
        for h in 0..self.dict.partials(i) {
            let w = 1.0 / ((h + 1) * (h + 1)) as f64;
            total += w;
            alone += w * f64::from(self.dict.alone(i, h, others));
        }
        if total > 0.0 { alone / total } else { 0.0 }
    }

    /// Whether a string at the level of the others sounds near the partials of expected
    /// atom `i`, within twice the per-atom range: then its absence cannot be established,
    /// since the string may only be further off its pitch than the tuning fit follows.
    /// Only partials with no partial of another expected atom that near are looked at;
    /// weights `1/h²` as for `obs`.
    fn heard_nearby(&self, i: usize, exp_mask: u64) -> bool {
        let cfg = &self.cfg;
        let grid = self.dict.grid();
        let df = grid.df();
        let lobe = 2.0 * grid.fs / grid.win as f64;
        let span = 2.0 * f64::from(cfg.string_range_cents);
        let others = exp_mask & !(1u64 << i);
        let mut lv = [0.0f64; MAX_ATOMS];
        let mut n = 0usize;
        for j in atoms(others) {
            lv[n] = self.level(j);
            n += 1;
        }
        if n == 0 {
            return false;
        }
        let lv = &mut lv[..n];
        lv.sort_unstable_by(f64::total_cmp);
        let reference = 0.5 * (lv[(n - 1) / 2] + lv[n / 2]);
        let (mut total, mut heard) = (0.0f64, 0.0f64);
        for h in 0..self.dict.partials(i).min(TUNE_PARTIALS) {
            let order = (h + 1) as f64;
            let f = order * self.dict.f0(i);
            let reach = f * ((span / 1200.0).exp2() - 1.0);
            let near = atoms(others).fold(f64::INFINITY, |m, j| {
                let fj = self.dict.f0(j);
                let hj = (f / fj).round().max(1.0);
                m.min((f - hj * fj).abs())
            });
            if near < reach + lobe {
                continue;
            }
            let p = f / df - grid.k0 as f64;
            let r = (reach / df).ceil().max(1.0);
            let (lo, hi) = ((p - r).round(), (p + r).round());
            if lo < 1.0 || hi > grid.nb as f64 - 2.0 {
                continue;
            }
            let (lo, hi) = (lo as usize, hi as usize);
            let y = &self.y;
            let k = (lo..=hi).fold(lo, |b, k| if y[k] > y[b] { k } else { b });
            let w = 1.0 / (order * order);
            total += w;
            // A peak inside the window, not the skirt of a partial outside it.
            let peak = k > lo && k < hi && y[k] >= y[k - 1] && y[k] >= y[k + 1];
            if peak && f64::from(y[k]) >= f64::from(cfg.mask_ratio) * reference / order {
                heard += w;
            }
        }
        total > 0.0 && heard >= 0.5 * total
    }

    /// `(obs, clear)` of expected atom `i`: see [`StringStat`].
    fn observability(&self, i: usize, exp_mask: u64) -> (f64, f64) {
        let cfg = &self.cfg;
        let grid = self.dict.grid();
        // Level a struck string would have: the median level of the other expected atoms.
        let mut others = [0.0f64; MAX_ATOMS];
        let mut n = 0usize;
        for j in atoms(exp_mask & !(1u64 << i)) {
            others[n] = self.level(j);
            n += 1;
        }
        let others = &mut others[..n];
        others.sort_unstable_by(f64::total_cmp);
        let reference = if n == 0 {
            0.0
        } else {
            0.5 * (others[(n - 1) / 2] + others[n / 2])
        };
        let (mut total, mut obs, mut clear) = (0.0f64, 0.0f64, 0.0f64);
        for h in 0..self.dict.partials(i) {
            let order = (h + 1) as f64;
            let w = 1.0 / (order * order);
            total += w;
            let alone = f64::from(self.dict.alone(i, h, exp_mask));
            obs += w * alone;
            let hz = order * self.dict.f0(i);
            if hz < cfg.obs_lo_hz {
                continue;
            }
            // What was already sounding at the partial, against the level it would have.
            let k = ((hz / grid.df()).round() as usize).saturating_sub(grid.k0);
            let lo = k.saturating_sub(1).min(grid.nb - 1);
            let hi = (k + 1).min(grid.nb - 1);
            let floor = self.pre[lo..=hi].iter().copied().fold(0.0, f32::max);
            let covered =
                f64::from(cfg.ring_gain * floor) >= f64::from(cfg.mask_ratio) * reference / order;
            if !covered {
                clear += w * alone;
            }
        }
        if total > 0.0 {
            (obs / total, clear / total)
        } else {
            (0.0, 0.0)
        }
    }

    /// Energy the first [`TUNE_PARTIALS`] partials of atom `i` would capture with the atom
    /// `cents` off its nominal pitch.
    fn tune_energy(&self, i: usize, cents: f64, partials: u64) -> f64 {
        let f0 = self.dict.nominal(i) * (cents / 1200.0).exp2();
        let top = self.dict.grid().top();
        let mut e = 0.0f64;
        for h in 0..TUNE_PARTIALS {
            let hz = (h + 1) as f64 * f0;
            if hz >= top {
                break;
            }
            if partials & (1u64 << h) != 0 {
                e += self.dict.capture(&self.y, hz);
            }
        }
        e
    }

    /// Guitar-wide offset, cents, and whether it is clear: every atom of the dictionary
    /// (expected and alternatives alike, so no hypothesis is favoured) is moved by the same
    /// offset, and the offset that captures the most new energy wins. Coarse scan at
    /// [`TUNE_STEP`] around `ref_cents`, then a one-cent scan around the best step and a
    /// parabola through the best three. Returns the offset, the peak over the median of the
    /// coarse scan, and whether no rival peak stands far from the best.
    fn fit_offset(&self, n_atoms: usize) -> (f64, f64, bool) {
        let cfg = &self.cfg;
        let r = f64::from(cfg.ref_cents);
        let half = ((f64::from(cfg.tune_range_cents) / TUNE_STEP).round() as usize)
            .min((MAX_TUNE_STEPS - 1) / 2);
        let n = 2 * half + 1;
        let at = |k: usize| r + (k as f64 - half as f64) * TUNE_STEP;
        let total = |c: f64| {
            (0..n_atoms)
                .map(|i| self.tune_energy(i, c, u64::MAX))
                .sum::<f64>()
        };
        let mut s = [0.0f64; MAX_TUNE_STEPS];
        for (k, v) in s[..n].iter_mut().enumerate() {
            *v = total(at(k));
        }
        let best = (0..n).fold(0, |b, k| if s[k] > s[b] { k } else { b });
        let peak = s[best];
        let mut sorted = s;
        sorted[..n].sort_unstable_by(f64::total_cmp);
        let median = sorted[n / 2];
        let sharp = if median > 0.0 { peak / median } else { 0.0 };
        // A rival: another local maximum, far from the best, nearly as high.
        let rival = (0..n).any(|k| {
            let local = (k == 0 || s[k] >= s[k - 1]) && (k + 1 == n || s[k] >= s[k + 1]);
            local
                && k != best
                && (at(k) - at(best)).abs() > RIVAL_CENTS
                && s[k] >= RIVAL_SHARE * peak
        });
        if best == 0 || best + 1 == n {
            return (at(best), sharp, !rival);
        }
        // Fine scan, one cent apart, over the neighbouring steps.
        const SPAN: usize = TUNE_STEP as usize;
        let mut fine = [0.0f64; 2 * SPAN + 1];
        let off = |j: usize| at(best) + j as f64 - SPAN as f64;
        for (j, v) in fine.iter_mut().enumerate() {
            *v = total(off(j));
        }
        let m = fine.len();
        let b = (0..m).fold(0, |b, k| if fine[k] > fine[b] { k } else { b });
        let mut c = off(b);
        if b > 0 && b + 1 < m {
            let (l, mid, h) = (fine[b - 1], fine[b], fine[b + 1]);
            let den = l - 2.0 * mid + h;
            if den < 0.0 {
                c += (0.5 * (l - h) / den).clamp(-0.5, 0.5);
            }
        }
        (c, sharp, !rival)
    }

    /// Own offset of atom `i` around the guitar-wide offset `delta`, cents, and whether it
    /// reached the edge of its range. Every partial of `i` (up to [`TUNE_PARTIALS`]) with no
    /// partial of an atom of `others` within the range and a main lobe gives a reading: the peak of the
    /// new energy within the range, placed between bins by a parabola through the log
    /// magnitudes. The atom takes the weighted median of the readings (weight: energy times
    /// order², the precision of a reading in cents) and the total weight. When more weight
    /// peaks at one edge of the range than inside it, the string lies beyond the range: the
    /// offset is that edge and the flag is set. NaN without a reading.
    fn fit_atom_offset(&self, i: usize, delta: f64, others: u64, strict: bool) -> Reading {
        let range = f64::from(self.cfg.string_range_cents);
        if range <= 0.0 {
            return Reading::Nothing;
        }
        let grid = self.dict.grid();
        let df = grid.df();
        let f0 = self.dict.nominal(i) * (delta / 1200.0).exp2();
        let y = &self.y[..grid.nb];
        let mut reads = [(0.0f64, 0.0f64); TUNE_PARTIALS];
        let mut n = 0usize;
        let mut total = 0.0f64;
        let (mut below, mut above) = (0.0f64, 0.0f64);
        // Half-width of the main lobe, Hz.
        let lobe = 2.0 * grid.fs / grid.win as f64;
        // Peak of `y` between two band bins, if it is a peak strictly inside them:
        // (bin, value, offset from the bin by a parabola through the log magnitudes).
        let peak = |lo: usize, hi: usize| -> Option<(usize, f32, f64)> {
            let k = (lo..=hi).fold(lo, |b, k| if y[k] > y[b] { k } else { b });
            if k == lo || k == hi {
                return None;
            }
            let (a, b, c) = (y[k - 1], y[k], y[k + 1]);
            if !(a > 0.0 && b > 0.0 && c > 0.0 && b >= a && b >= c) {
                return None;
            }
            let (la, lb, lc) = (f64::from(a).ln(), f64::from(b).ln(), f64::from(c).ln());
            let den = la - 2.0 * lb + lc;
            let d = if den < 0.0 {
                (0.5 * (la - lc) / den).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            Some((k, b, d))
        };
        let window = |f: f64, cents: f64| -> Option<(usize, usize, f64)> {
            let reach = f * ((cents / 1200.0).exp2() - 1.0);
            let p = f / df - grid.k0 as f64;
            let r = (reach / df).ceil().max(1.0);
            let (lo, hi) = ((p - r).round(), (p + r).round());
            if lo < 1.0 || hi > grid.nb as f64 - 2.0 {
                return None;
            }
            Some((lo as usize, hi as usize, reach))
        };
        for h in 0..self.dict.partials(i).min(TUNE_PARTIALS) {
            let f = (h + 1) as f64 * f0;
            // Distance to the nearest partial of another atom, Hz.
            let near = atoms(others).fold(f64::INFINITY, |m, j| {
                let fj = self.dict.f0(j) * (delta / 1200.0).exp2();
                let hj = (f / fj).round().max(1.0);
                m.min((f - hj * fj).abs())
            });
            let Some((lo, hi, reach)) = window(f, range) else {
                continue;
            };
            // No partial of another atom may sit within the search or a lobe around it.
            if near < reach + lobe {
                continue;
            }
            let order2 = ((h + 1) as f64).powi(2);
            if let Some((k, b, d)) = peak(lo, hi) {
                let hz = (grid.k0 as f64 + k as f64 + d) * df;
                let cents = 1200.0 * (hz / f).log2();
                let wgt = f64::from(b) * f64::from(b) * order2;
                reads[n] = (cents, wgt);
                total += wgt;
                n += 1;
                continue;
            }
            // No peak inside: is there one just beyond, in twice the range?
            let Some((wlo, whi, wreach)) = window(f, 2.0 * range) else {
                continue;
            };
            if near < wreach + lobe {
                continue;
            }
            if let Some((k, b, _)) = peak(wlo, whi) {
                let wgt = f64::from(b) * f64::from(b) * order2;
                if k < lo {
                    below += wgt;
                } else if k > hi {
                    above += wgt;
                }
            }
        }
        if below > total.max(above) {
            return Reading::Unsure(-range);
        }
        if above > total.max(below) {
            return Reading::Unsure(range);
        }
        if n == 0 || total <= 0.0 {
            return Reading::Nothing;
        }
        let eps = weighted_median(reads[..n].iter().copied());
        // In `strict` mode the partials must agree, as those of one sounding string do;
        // readings that do not are the skirts of other strings.
        if strict {
            let agree: f64 = reads[..n]
                .iter()
                .filter(|r| (r.0 - eps).abs() <= AGREE_CENTS)
                .map(|r| r.1)
                .sum();
            if agree < AGREE_SHARE * total || n < AGREE_MIN_PARTIALS {
                return Reading::Unsure(0.0);
            }
        }
        if eps.abs() >= range {
            return Reading::Unsure(eps.clamp(-range, range));
        }
        Reading::Read(eps, total)
    }

    /// Offset of every expected atom from `delta` (see [`CombWitness::fit_atom_offset`]):
    /// on the partials no other expected atom comes near, or, for an atom without such a
    /// partial (an octave above another one), on the places it shares with the atoms of its
    /// own pitch class. Fills `eps` and `weight` for the expected atoms (0 without a
    /// reading, or a weight of 0 when the reading is unsure).
    fn fit_atom_offsets(
        &self,
        exp_mask: u64,
        delta: f64,
        eps: &mut [f64; MAX_ATOMS],
        weight: &mut [f64; MAX_ATOMS],
    ) {
        for i in atoms(exp_mask) {
            let class = self.dict.pitch(i).rem_euclid(12);
            let others = exp_mask & !(1u64 << i);
            let mut read = self.fit_atom_offset(i, delta, others, false);
            if read == Reading::Nothing {
                let foreign = atoms(others)
                    .filter(|&j| self.dict.pitch(j).rem_euclid(12) != class)
                    .fold(0u64, |m, j| m | (1u64 << j));
                read = self.fit_atom_offset(i, delta, foreign, false);
            }
            (eps[i], weight[i]) = match read {
                Reading::Read(e, w) => (e, w),
                Reading::Unsure(e) => (e, 0.0),
                Reading::Nothing => (0.0, 0.0),
            };
        }
    }

    fn run(&mut self, w: &Window, set: &ConfusionSet) -> Result<Opinion, Fault> {
        let cfg = self.cfg;
        let n_alts = set.alts.len();
        if n_alts > MAX_ALTS {
            return Err(Fault::BadSet);
        }

        // ---- windows -------------------------------------------------------------------
        let st = stages(w.fs).ok_or(Fault::BadRate)?;
        let fs_a = w.fs / f64::from(1u32 << st);
        let win = (w.post.len() >> st).min(N_FFT);
        let ms = |n: usize| n as f64 * 1000.0 / fs_a;
        if win == 0 || ms(win) < cfg.min_post_ms {
            return Err(Fault::PostShort);
        }
        // `pre` is never analysed with a longer window than `post`: a narrower lobe would
        // leave the skirts of a ringing partial in `post` looking new.
        let pre_win = (w.pre.len() >> st).min(win);
        if pre_win == 0 || ms(pre_win) < cfg.min_pre_ms {
            return Err(Fault::PreShort);
        }
        let post = &w.post[..win << st];
        let pre = &w.pre[w.pre.len() - (pre_win << st)..];
        let (post_sq, post_clip, post_ok) = scan(post, cfg.clip_level);
        let (_, pre_clip, pre_ok) = scan(pre, cfg.clip_level);
        if !(post_ok && pre_ok) {
            return Err(Fault::NonFinite);
        }
        let clipped = (post_clip + pre_clip) as f32 / (post.len() + pre.len()) as f32;
        if clipped > cfg.clip_frac {
            return Err(Fault::Clipped);
        }
        let rms_db = 10.0 * (post_sq / post.len() as f64).max(1e-30).log10();
        if rms_db < f64::from(cfg.min_level_dbfs) {
            return Err(Fault::Silent);
        }
        // A window too short for the dictionary's columns is too short for the witness.
        let grid = Grid::new(fs_a, win, cfg.f_lo, cfg.f_hi).ok_or(Fault::PostShort)?;
        let pre_grid = Grid {
            win: pre_win,
            ..grid
        };
        let nb = grid.nb;
        if !(self.fft.spectrum(post, st, win, &grid, &mut self.post)
            && self
                .fft
                .spectrum(pre, st, pre_win, &pre_grid, &mut self.pre))
        {
            return Err(Fault::BadRate);
        }

        // ---- new energy ----------------------------------------------------------------
        let (mut e_new, mut e_pre) = (0.0f64, 0.0f64);
        for k in 0..nb {
            let v = (self.post[k] - cfg.ring_gain * self.pre[k]).max(0.0);
            self.y[k] = v;
            e_new += f64::from(v) * f64::from(v);
            e_pre += f64::from(self.pre[k]) * f64::from(self.pre[k]);
        }
        if !e_new.is_finite() || !e_pre.is_finite() {
            return Err(Fault::NonFinite);
        }
        if e_new <= 0.0 {
            return Err(Fault::Silent);
        }
        self.e_new = e_new;
        let n_eff = grid.n_eff();
        let snr_db = (10.0 * (e_new / e_pre.max(1e-30)).log10()).clamp(-200.0, 200.0);
        self.last.snr_db = snr_db as f32;
        self.last.n_eff = n_eff as f32;
        self.last.n_alts = n_alts;

        // ---- dictionary ----------------------------------------------------------------
        self.dict.reset(grid);
        let (exp_mask, exp_atom) = self.atoms_of(set, &set.expected.sounding)?;
        if exp_mask == 0 {
            return Err(Fault::BadSet);
        }
        let mut alt_mask = [0u64; MAX_ALTS];
        for (i, h) in set.alts.iter().enumerate() {
            alt_mask[i] = self.atoms_of(set, &h.sounding)?.0;
        }

        // ---- tuning: where the guitar is, then where each atom is ---------------------
        let n_atoms = self.dict.len();
        let (delta, sharp, no_rival) = self.fit_offset(n_atoms);
        self.last.offset_cents = delta as f32;
        self.last.tune_sharp = sharp as f32;
        let edge = f64::from(cfg.tune_range_cents) - TUNE_STEP;
        if (delta - f64::from(cfg.ref_cents)).abs() >= edge.max(TUNE_STEP) {
            return Err(Fault::Detuned);
        }
        // Every expected atom reads its own offset around `delta`. The loudest string sets
        // `delta`, so the offsets are read twice: the second time around their median over
        // the atoms (each atom one vote, so one string a loud neighbour pulls cannot move
        // the guitar), which is where most strings are.
        let mut eps = [0.0f64; MAX_ATOMS];
        let mut weight = [0.0f64; MAX_ATOMS];
        self.fit_atom_offsets(exp_mask, delta, &mut eps, &mut weight);
        let mid = weighted_median(
            atoms(exp_mask).map(|i| (eps[i], if weight[i] > 0.0 { 1.0 } else { 0.0 })),
        );
        let delta = if mid.abs() > RECENTRE_CENTS {
            let d = delta + mid;
            self.fit_atom_offsets(exp_mask, d, &mut eps, &mut weight);
            d
        } else {
            delta
        };
        self.last.offset_cents = delta as f32;
        if (delta - f64::from(cfg.ref_cents)).abs() >= edge.max(TUNE_STEP) {
            return Err(Fault::Detuned);
        }
        let eps_max = atoms(exp_mask).fold(
            0.0f64,
            |m, i| if eps[i].abs() > m.abs() { eps[i] } else { m },
        );
        // An alternative takes the offset of the nearest expected atom of its pitch class:
        // octaves keep their partials in the same place. Any other alternative reads its own
        // offset, but only from partials that agree: a string that sounds is placed, one
        // that does not stays with the guitar.
        for i in 0..n_atoms {
            if exp_mask & (1u64 << i) == 0 {
                let p = self.dict.pitch(i);
                let mate = atoms(exp_mask)
                    .filter(|&j| (self.dict.pitch(j) - p).rem_euclid(12) == 0)
                    .min_by_key(|&j| (self.dict.pitch(j) - p).abs());
                eps[i] = match mate {
                    Some(j) => eps[j],
                    None => match self.fit_atom_offset(i, delta, exp_mask, true) {
                        Reading::Read(e, _) => e,
                        _ => 0.0,
                    },
                };
            }
        }
        for (i, e) in eps[..n_atoms].iter().enumerate() {
            self.dict.retune(i, delta + e);
        }
        self.dict.tie(exp_mask, TIE_BINS * fs_a / win as f64);
        self.last.atom_cents_max = eps_max as f32;
        let tune_ok = sharp >= f64::from(cfg.tune_sharp_min) && no_rival;
        // Partials of each atom that sit on a partial of each other atom: closer than the
        // columns can tell apart, or closer than a string may be off its own pitch.
        let ratio = (GUARD_CENTS / 1200.0).exp2() - 1.0;
        for i in 0..self.dict.len() {
            for j in 0..self.dict.len() {
                let mut bits = 0u64;
                if i != j {
                    let fj = self.dict.f0(j);
                    let added = exp_mask & (1u64 << i) == 0;
                    for h in 0..self.dict.partials(i) {
                        let f = (h + 1) as f64 * self.dict.f0(i);
                        let near = (f / fj).round().max(1.0) * fj;
                        if self.dict.alone(i, h, 1u64 << j) < cfg.min_alone
                            || (added && (f - near).abs() < ratio * near)
                        {
                            bits |= 1u64 << h;
                        }
                    }
                }
                self.coll[i * MAX_ATOMS + j] = bits;
            }
        }
        self.memo_n = 0;
        self.have_exp = false;

        // ---- expected hypothesis and per-string statistics ------------------------------
        let lam = f64::from(cfg.atom_penalty);
        let floor = f64::from(cfg.model_floor) * e_new;
        let g = |num: f64, den: f64| n_eff * ((num + floor) / (den + floor)).ln();
        let rss_e = self.fit(exp_mask, exp_mask);
        let explained = 1.0 - rss_e / e_new;
        self.last.explained = explained as f32;

        // Expected atoms that can show themselves (`seen`) and whose absence could be
        // established (`clear`).
        let (mut seen_mask, mut clear_mask) = (0u64, 0u64);
        let mut g_exp = [0.0f64; 6];
        let mut obs = [(0.0f64, 0.0f64); 6];
        for s in 0..6 {
            let i = exp_atom[s];
            // Two strings on one note: taking one away changes nothing the fit can see.
            if i == NO_ATOM || (0..6).any(|t| t != s && exp_atom[t] == i) {
                continue;
            }
            obs[s] = self.observability(usize::from(i), exp_mask);
            if obs[s].0 >= f64::from(cfg.obs_min) {
                seen_mask |= 1u64 << i;
            }
            if obs[s].1 >= f64::from(cfg.obs_min) && !self.heard_nearby(usize::from(i), exp_mask) {
                clear_mask |= 1u64 << i;
            }
            let without = exp_mask & !(1u64 << i);
            let rss = self.fit(without, exp_mask);
            g_exp[s] = g(rss, self.fit(exp_mask, without));
        }
        // What a missing `Must` string would score, over the strings that can be seen and
        // over those whose absence could be established.
        let (mut miss_seen, mut miss_clear) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut n_seen = 0u32;
        for s in 0..6 {
            let i = exp_atom[s];
            if i == NO_ATOM || !matches!(set.ask[s], Ask::Must(_)) {
                continue;
            }
            let lead = lam - 0.5 * g_exp[s];
            if seen_mask & (1u64 << i) != 0 {
                n_seen += 1;
                miss_seen = miss_seen.max(lead);
            }
            if clear_mask & (1u64 << i) != 0 {
                miss_clear = miss_clear.max(lead);
            }
        }

        // ---- alternative atoms, one at a time (the veto) ---------------------------------
        let mut g_add = [0.0f64; MAX_ATOMS];
        let mut veto = f64::NEG_INFINITY;
        let all = (1u64 << self.dict.len()) - 1;
        for i in atoms(all & !exp_mask) {
            let rss = self.fit(exp_mask | (1u64 << i), exp_mask);
            g_add[i] = g(rss_e, rss);
            veto = veto.max(0.5 * g_add[i] - lam);
        }
        self.last.veto = veto.clamp(-1e6, 1e6) as f32;
        let mut g_alt = [0.0f64; 6];
        let mut has_alt = [false; 6];
        for h in &set.alts {
            let s = match h.kind {
                HypKind::WrongFret { string, .. } | HypKind::Extra { string } => {
                    usize::from(string)
                }
                _ => continue,
            };
            if s >= 6 || h.sounding[s] < 0 {
                continue;
            }
            let i = usize::from(self.atom(set, s, h.sounding[s])?);
            has_alt[s] = true;
            if exp_mask & (1u64 << i) == 0 {
                g_alt[s] = g_alt[s].max(g_add[i]);
            }
        }
        for s in 0..6 {
            if exp_atom[s] != NO_ATOM || has_alt[s] {
                self.last.strings[s] = Some(StringStat {
                    g_exp: g_exp[s].clamp(-1e6, 1e6) as f32,
                    g_alt: g_alt[s].clamp(-1e6, 1e6) as f32,
                    obs: obs[s].0 as f32,
                    clear: obs[s].1 as f32,
                });
            }
        }

        // ---- every alternative against the expected hypothesis ---------------------------
        // `block`: what stands in the way of `Supports`. `hard`: the same without the
        // strings that can be seen but whose absence cannot be established.
        let (mut block, mut hard) = (miss_seen.max(veto), miss_clear.max(veto));
        let mut score = [f64::NEG_INFINITY; MAX_ALTS];
        let mut top = f64::NEG_INFINITY;
        for i in 0..n_alts {
            let mask = alt_mask[i];
            let rss = self.fit(mask, exp_mask);
            let llr = 0.5 * g(self.fit(exp_mask, mask), rss);
            let removed = exp_mask & !mask;
            // An added atom costs λ only if it can show itself next to the other atoms of
            // the alternative: an octave above another of them brings no partial of its own.
            let added = f64::from(
                atoms(mask & !exp_mask)
                    .filter(|&j| self.share_alone(j, mask) >= f64::from(cfg.obs_min))
                    .count() as u32,
            );
            let credit = |m: u64| llr + lam * (f64::from((removed & m).count_ones()) - added);
            score[i] = credit(clear_mask);
            self.last.score[i] = score[i].clamp(-1e6, 1e6) as f32;
            top = top.max(score[i]);
            if added > 0.0 {
                block = block.max(score[i]);
                hard = hard.max(score[i]);
            } else {
                // Nothing added: it only competes through strings that can be seen.
                if removed & seen_mask != 0 {
                    block = block.max(credit(seen_mask));
                }
                if removed & clear_mask != 0 {
                    hard = hard.max(score[i]);
                }
            }
        }
        self.last.block = block.clamp(-1e6, 1e6) as f32;
        if self.last.capped {
            return Ok(Opinion::Abstains(Abstain::Ambiguous));
        }

        self.last.tune_ok = tune_ok;

        // ---- opinion -----------------------------------------------------------------------
        let (m_name, m_sup, min_expl) = (
            f64::from(cfg.name_margin),
            f64::from(cfg.support_margin),
            f64::from(cfg.min_explained),
        );
        if top >= m_name && snr_db < f64::from(cfg.name_snr_db) {
            return Ok(Opinion::Abstains(Abstain::LowSnr));
        }
        if top >= m_name {
            // Everything within the tie window has to be something this witness may name
            // and has to explain the new energy; otherwise the answer is not a name.
            let mut alts = 0u32;
            let mut ok = true;
            for i in 0..n_alts {
                if score[i] < top - f64::from(cfg.tie_margin) || score[i] <= 0.0 {
                    continue;
                }
                let mask = alt_mask[i];
                let removed = exp_mask & !mask;
                let nameable = mask != 0 && set.alts[i].kind != HypKind::AllMuted;
                let rss = self.fit(mask, mask);
                // The expected atoms it drops must not be needed next to its own.
                let consistent = removed == 0 || mask & !exp_mask == 0 || {
                    let both = self.fit(mask | exp_mask, mask);
                    0.5 * g(rss, both) <= lam * f64::from(removed.count_ones())
                };
                ok &= nameable && consistent && 1.0 - rss / e_new >= min_expl;
                alts |= 1u32 << i;
            }
            return Ok(if ok && alts != 0 && !self.last.capped && tune_ok {
                Opinion::Names {
                    alts,
                    margin: top.clamp(-1e6, 1e6) as f32,
                }
            } else {
                Opinion::Abstains(Abstain::Ambiguous)
            });
        }
        if n_seen > 0 && block <= -m_sup && explained >= min_expl {
            return Ok(Opinion::Supports {
                margin: (-block).clamp(-1e6, 1e6) as f32,
            });
        }
        Ok(Opinion::Abstains(if snr_db < f64::from(cfg.snr_min_db) {
            Abstain::LowSnr
        } else if explained >= min_expl && hard <= -m_sup {
            Abstain::Unobservable
        } else {
            Abstain::Ambiguous
        }))
    }
}

impl Witness for CombWitness {
    fn family(&self) -> Family {
        Family::Dsp
    }

    fn name(&self) -> &'static str {
        "comb"
    }

    fn opine(&mut self, w: &Window, set: &ConfusionSet) -> Opinion {
        self.analyze(w, set).opinion
    }
}
