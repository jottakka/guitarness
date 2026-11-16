//! DSP witness against `guitarness-sim` strums (the verifier/learner design plan, unit V2).
//!
//! Everything here is synthetic: harmonic Karplus-Strong strings, a Schroeder reverb, pink
//! noise plus hum and a laptop-microphone high-pass. Seeds stay inside
//! `guitarness_verify::excite::DEV_SEEDS`. Tests (1)-(8) follow the V2 list of the plan;
//! `dev_sweep` (ignored) is the grid the operating point was chosen on.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

use guitarness_dsp::synth::Mulberry32;
use guitarness_sim::fx::{hum, laptop_mic, pink, reverb, rms};
use guitarness_sim::strum::{
    Dir, OPEN, StrumSpec, Voicing, render_take, render_take_with, voicing,
};
use guitarness_verify::excite::{
    Analysis, CombWitness, DEV_SEEDS, ExciteConfig, Fault, MAX_ALTS, cut,
};
use guitarness_verify::types::{
    Abstain, Ask, ConfusionSet, Family, HypKind, Hypothesis, Opinion, Window, Witness,
};

// ---------------------------------------------------------------------------------------
// Allocation counter (per thread, so tests may run in parallel).

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn bump() {
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocs() -> u64 {
    ALLOCS.with(|c| c.get())
}

// ---------------------------------------------------------------------------------------
// Synthetic strums

const SR: f64 = 48_000.0;
const CHORDS: [&str; 9] = ["G", "C", "D", "Em", "Am", "F", "A", "E", "Dm"];
/// Level of the judged strum (RMS of its first 50 ms), as in `guitarness_sim::timing`.
const STRUM_RMS: f64 = 0.1;

/// What the hand really does, against what the chart asks.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hand {
    Right,
    Missing(usize),
    Fret(usize, i8),
    Extra(usize),
    Chord(&'static str),
    Muted,
}

#[derive(Clone, Copy, Debug)]
struct Scene {
    chord: &'static str,
    /// Chord strummed `gap` seconds earlier and left ringing.
    prev: Option<&'static str>,
    /// Level of that earlier strum against the judged one.
    prev_gain: f64,
    gap: f64,
    hand: Hand,
    dir: Dir,
    spread: f64,
    snr_db: f64,
    wet: f64,
    laptop: bool,
    /// One more string struck with the chord: (string, fret, gain against the chord).
    planted: Option<(usize, i8, f64)>,
    seed: u32,
    /// Tuning error of each string, cents (the simulator tunes 440 Hz exactly; each string
    /// is moved by resampling it).
    cents: [f64; 6],
}

impl Scene {
    fn clean(chord: &'static str, seed: u32) -> Self {
        Self {
            chord,
            prev: None,
            prev_gain: 1.0,
            gap: 0.5,
            hand: Hand::Right,
            dir: Dir::Down,
            spread: 0.015,
            snr_db: 40.0,
            wet: 0.0,
            laptop: false,
            planted: None,
            seed,
            cents: [0.0; 6],
        }
    }
}

struct Take {
    audio: Vec<f32>,
    attack: usize,
    last: usize,
    prev_end: usize,
}

impl Take {
    fn window(&self) -> Window<'_> {
        cut(
            &self.audio,
            SR,
            self.attack,
            self.last,
            self.prev_end,
            self.audio.len(),
        )
    }
}

fn reach(dir: Dir) -> (usize, usize) {
    match dir {
        Dir::Down => (0, 5),
        Dir::Up => (2, 5),
    }
}

fn render_scene(sc: &Scene) -> Take {
    let mut rng = Mulberry32::new(sc.seed.wrapping_mul(2_654_435_761).wrapping_add(17));
    let (lo, hi) = reach(sc.dir);
    let mut frets = voicing(sc.chord).0;
    let mut strings = [false; 6];
    for (s, on) in strings.iter_mut().enumerate() {
        *on = (lo..=hi).contains(&s);
    }
    let mut muted = false;
    match sc.hand {
        Hand::Right => {}
        Hand::Missing(s) => strings[s] = false,
        Hand::Fret(s, f) => frets[s] = f,
        Hand::Extra(s) => frets[s] = 0,
        Hand::Chord(c) => frets = voicing(c).0,
        Hand::Muted => muted = true,
    }
    let t_prev = 0.30;
    let t0 = if sc.prev.is_some() {
        t_prev + sc.gap
    } else {
        0.45
    };
    let mut specs = Vec::new();
    if let Some(p) = sc.prev {
        specs.push(StrumSpec {
            t0: t_prev,
            voicing: voicing(p),
            dir: Dir::Down,
            spread: 0.02,
            gain: sc.prev_gain,
            muted: false,
            strings: [true; 6],
            attack: 0.0,
        });
    }
    specs.push(StrumSpec {
        t0,
        voicing: Voicing(frets),
        dir: sc.dir,
        spread: sc.spread,
        gain: 1.0,
        muted,
        strings,
        attack: 0.001,
    });
    if let Some((s, fret, gain)) = sc.planted {
        let mut v = [-1i8; 6];
        v[s] = fret;
        let mut only = [false; 6];
        only[s] = true;
        specs.push(StrumSpec {
            t0,
            voicing: Voicing(v),
            dir: sc.dir,
            spread: 0.0,
            gain,
            muted: false,
            strings: only,
            attack: 0.001,
        });
    }
    let len = ((t0 + sc.spread + 0.30) * SR) as usize;
    let (mut audio, string_times) = if sc.cents == [0.0; 6] || sc.hand == Hand::Muted {
        let r = render_take(SR, &specs, len, &mut rng);
        (r.audio, r.string_times)
    } else {
        render_detuned(&specs, len, &mut rng, &sc.cents)
    };
    let attack = (t0 * SR).round() as usize;
    // the judged strum comes after the ringing one, before the planted string
    let main = usize::from(sc.prev.is_some());
    let last = string_times
        .get(main)
        .and_then(|t| t.iter().copied().reduce(f64::max))
        .map_or(attack, |t| (t * SR).round() as usize);
    let level = rms(&audio[attack..attack + (0.05 * SR) as usize]).max(1e-9);
    audio
        .iter_mut()
        .for_each(|v| *v = (*v as f64 * STRUM_RMS / level) as f32);
    reverb(&mut audio, SR, 0.5, sc.wet);
    let noise_rms = STRUM_RMS * 10f64.powf(-sc.snr_db / 20.0);
    let n = pink(len, &mut rng);
    let h = hum(len, SR, 60.0);
    for ((a, nv), hv) in audio.iter_mut().zip(&n).zip(&h) {
        *a += (noise_rms * (*nv as f64 + 0.5 * *hv as f64)) as f32;
    }
    if sc.laptop {
        laptop_mic(&mut audio, SR);
    }
    let prev_end = if sc.prev.is_some() {
        ((t_prev + 0.02 + 0.010) * SR) as usize
    } else {
        0
    };
    Take {
        audio,
        attack,
        last,
        prev_end,
    }
}

/// Cubic (Catmull-Rom) resample: pitch moves by `ratio` (> 1 = higher, shorter).
fn repitch(x: &[f32], ratio: f64) -> Vec<f32> {
    let n = (x.len() as f64 / ratio).floor() as usize;
    let at = |i: isize| x.get(i.max(0) as usize).copied().unwrap_or(0.0) as f64;
    (0..n)
        .map(|k| {
            let p = k as f64 * ratio;
            let i = p.floor() as isize;
            let t = p - i as f64;
            let (a, b, c, d) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            (b + 0.5
                * t
                * (c - a + t * (2.0 * a - 5.0 * b + 4.0 * c - d + t * (3.0 * (b - c) + d - a))))
                as f32
        })
        .collect()
}

/// Render as `render_take` (same random draws), then rebuild the mix with every string moved
/// by its own cents (as the V4b detune sweep does). Only for strums that ring.
fn render_detuned(
    specs: &[StrumSpec],
    len: usize,
    rng: &mut Mulberry32,
    cents: &[f64; 6],
) -> (Vec<f32>, Vec<Vec<f64>>) {
    let mut owner: Vec<usize> = Vec::new();
    for sp in specs {
        assert!(!sp.muted, "detune is for strums that ring");
        owner.extend(sp.order());
    }
    let mut parts: Vec<(usize, Vec<f32>)> = Vec::new();
    let r = render_take_with(SR, specs, len, rng, |_, start, y| {
        parts.push((start, y.to_vec()));
    });
    assert_eq!(parts.len(), owner.len(), "one part per string struck");
    let mut audio = vec![0.0f32; len];
    for ((start, y), s) in parts.iter().zip(owner) {
        let y = repitch(y, 2f64.powf(cents[s] / 1200.0));
        for (i, v) in y.iter().enumerate() {
            if let Some(a) = audio.get_mut(start + i) {
                *a += v;
            }
        }
    }
    (audio, r.string_times)
}

/// Per-string cents: one offset for the whole guitar plus a Gaussian draw of `sigma` per
/// string, from `seed`.
fn detune(offset: f64, sigma: f64, seed: u32) -> [f64; 6] {
    let mut r = Mulberry32::new(seed.wrapping_mul(40_503).wrapping_add(0x9E37));
    let mut c = [offset; 6];
    for v in &mut c {
        let (u1, u2) = (r.next_f64().max(1e-12), r.next_f64());
        *v += sigma * (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
    }
    c
}

/// The closed set a chart gives for `chord` under a stroke `dir`, with the chords before and
/// after it as neighbours. Hand-built with the rules of plan section 2.4 (no `req` letters,
/// default span), so this file depends on `types.rs` only.
fn set_for(chord: &str, dir: Dir, prev: Option<&str>, next: Option<&str>) -> ConfusionSet {
    let frets = voicing(chord).0;
    let (lo, hi) = reach(dir);
    let mut ask = [Ask::May; 6];
    let mut base = [-1i8; 6];
    for s in lo..=hi {
        if frets[s] >= 0 {
            ask[s] = Ask::Must(frets[s] as u8);
            base[s] = frets[s];
        } else {
            ask[s] = Ask::MustNot;
        }
    }
    let hyp = |kind: HypKind, sounding: [i8; 6]| Hypothesis { kind, sounding };
    let (mut missing, mut wrong, mut extra, mut neigh) = (vec![], vec![], vec![], vec![]);
    for s in 0..6 {
        match ask[s] {
            Ask::Must(f) => {
                let mut m = base;
                m[s] = -1;
                missing.push(hyp(HypKind::Missing { string: s as u8 }, m));
                for alt in [f as i8 - 1, f as i8 + 1, 0] {
                    if alt >= 0 && alt != f as i8 {
                        let mut w = base;
                        w[s] = alt;
                        let kind = HypKind::WrongFret {
                            string: s as u8,
                            fret: alt as u8,
                        };
                        wrong.push(hyp(kind, w));
                    }
                }
            }
            Ask::MustNot => {
                let mut x = base;
                x[s] = 0;
                extra.push(hyp(HypKind::Extra { string: s as u8 }, x));
            }
            Ask::May => {}
        }
    }
    for c in [prev, next].into_iter().flatten().filter(|c| *c != chord) {
        let v = voicing(c).0;
        let mut snd = [-1i8; 6];
        snd[lo..=hi].copy_from_slice(&v[lo..=hi]);
        neigh.push(hyp(HypKind::Neighbour { chord: c.into() }, snd));
    }
    let muted = vec![hyp(HypKind::AllMuted, [-1; 6])];
    let mut seen = vec![base];
    let mut keep = |hs: Vec<Hypothesis>| -> Vec<Hypothesis> {
        hs.into_iter()
            .filter(|h| {
                let fresh = !seen.contains(&h.sounding);
                if fresh {
                    seen.push(h.sounding);
                }
                fresh
            })
            .collect()
    };
    let (missing, mut wrong, extra, neigh, muted) = (
        keep(missing),
        keep(wrong),
        keep(extra),
        keep(neigh),
        keep(muted),
    );
    wrong.truncate(24usize.saturating_sub(missing.len() + extra.len() + neigh.len() + muted.len()));
    let mut alts = missing;
    alts.extend(wrong);
    alts.extend(extra);
    alts.extend(neigh);
    alts.extend(muted);
    ConfusionSet {
        chord: chord.into(),
        ask,
        expected: hyp(HypKind::Expected, base),
        alts,
        tuning: OPEN,
        capo: 0,
    }
}

/// Index in `set.alts` of the hypothesis the hand realised (by what sounds).
fn truth(set: &ConfusionSet, sc: &Scene) -> Option<usize> {
    let (lo, hi) = reach(sc.dir);
    let mut snd = set.expected.sounding;
    match sc.hand {
        Hand::Right => return None,
        Hand::Missing(s) => snd[s] = -1,
        Hand::Fret(s, f) => snd[s] = f,
        Hand::Extra(s) => snd[s] = 0,
        Hand::Chord(c) => {
            snd = [-1; 6];
            snd[lo..=hi].copy_from_slice(&voicing(c).0[lo..=hi]);
        }
        Hand::Muted => snd = [-1; 6],
    }
    set.alts.iter().position(|h| h.sounding == snd)
}

fn run(w: &mut CombWitness, sc: &Scene, set: &ConfusionSet) -> Analysis {
    let take = render_scene(sc);
    *w.analyze(&take.window(), set)
}

// ---------------------------------------------------------------------------------------
// Development sweep: how the operating point in `ExciteConfig::default()` was chosen.
// ---------------------------------------------------------------------------------------

#[derive(Default, Clone)]
struct Tally {
    n: usize,
    supports: usize,
    names_true: usize,
    names_other: usize,
    low_snr: usize,
    unobservable: usize,
    ambiguous: usize,
    other: usize,
}

impl Tally {
    fn add(&mut self, a: &Analysis, truth: Option<usize>) {
        self.n += 1;
        match a.opinion {
            Opinion::Supports { .. } => self.supports += 1,
            Opinion::Names { alts, .. } => {
                if truth.is_some_and(|t| alts & (1 << t) != 0) {
                    self.names_true += 1;
                } else {
                    self.names_other += 1;
                }
            }
            Opinion::Abstains(Abstain::LowSnr) => self.low_snr += 1,
            Opinion::Abstains(Abstain::Unobservable) => self.unobservable += 1,
            Opinion::Abstains(Abstain::Ambiguous) => self.ambiguous += 1,
            Opinion::Abstains(_) => self.other += 1,
        }
    }

    fn merge(&mut self, o: &Tally) {
        self.n += o.n;
        self.supports += o.supports;
        self.names_true += o.names_true;
        self.names_other += o.names_other;
        self.low_snr += o.low_snr;
        self.unobservable += o.unobservable;
        self.ambiguous += o.ambiguous;
        self.other += o.other;
    }

    fn line(&self) -> String {
        format!(
            "n {:5}  supports {:5}  names(true) {:5}  names(other) {:4}  lowsnr {:4}  unobs {:5}  ambig {:5}  other {:3}",
            self.n,
            self.supports,
            self.names_true,
            self.names_other,
            self.low_snr,
            self.unobservable,
            self.ambiguous,
            self.other
        )
    }
}

fn cfg_from_env() -> ExciteConfig {
    let mut c = ExciteConfig::default();
    if let Ok(s) = std::env::var("EXCITE_CFG") {
        for kv in s.split(',').filter(|kv| !kv.is_empty()) {
            let (k, v) = kv.split_once('=').expect("key=value");
            let v: f64 = v.parse().expect("number");
            match k {
                "atom_penalty" => c.atom_penalty = v as f32,
                "support_margin" => c.support_margin = v as f32,
                "name_margin" => c.name_margin = v as f32,
                "tie_margin" => c.tie_margin = v as f32,
                "obs_min" => c.obs_min = v as f32,
                "min_alone" => c.min_alone = v as f32,
                "model_floor" => c.model_floor = v as f32,
                "name_snr_db" => c.name_snr_db = v as f32,
                "obs_lo_hz" => c.obs_lo_hz = v,
                "mask_ratio" => c.mask_ratio = v as f32,
                "min_explained" => c.min_explained = v as f32,
                "ring_gain" => c.ring_gain = v as f32,
                "dict_shift" => c.dict_shift = v as i32,
                "ref_cents" => c.ref_cents = v as f32,
                "tune_range_cents" => c.tune_range_cents = v as f32,
                "string_range_cents" => c.string_range_cents = v as f32,
                "tune_sharp_min" => c.tune_sharp_min = v as f32,
                _ => panic!("unknown key {k}"),
            }
        }
    }
    c
}

/// Conditions of the development grid for one chord and one seed.
fn conditions(chord: &'static str, ci: usize, seed: u32) -> Vec<Scene> {
    let mut out = Vec::new();
    let mut k = 0u32;
    for dir in [Dir::Down, Dir::Up] {
        for spread in [0.015, 0.060] {
            for snr_db in [40.0, 20.0] {
                for wet in [0.0, 0.25] {
                    for laptop in [false, true] {
                        for ctx in 0..3 {
                            k += 1;
                            let s = seed * 1000 + k;
                            let prev = match ctx {
                                0 => None,
                                1 => Some(chord),
                                _ => Some(CHORDS[(ci + 1 + (s as usize % 8)) % 9]),
                            };
                            out.push(Scene {
                                chord,
                                prev,
                                prev_gain: [0.5, 1.0, 2.0][(s as usize / 3) % 3],
                                gap: [0.33, 0.5, 1.0][(s as usize / 7) % 3],
                                hand: Hand::Right,
                                dir,
                                spread,
                                snr_db,
                                wet,
                                laptop,
                                planted: None,
                                seed: s,
                                cents: [0.0; 6],
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

fn next_of(ci: usize) -> &'static str {
    CHORDS[(ci + 4) % 9]
}

#[test]
#[ignore = "development sweep; run with --release -- --ignored --nocapture dev_sweep"]
fn dev_sweep() {
    let cfg = cfg_from_env();
    // `EXCITE_DETUNE=offset,sigma` (cents) detunes every strum of the grid.
    let (d_off, d_sig) = std::env::var("EXCITE_DETUNE").ok().map_or((0.0, 0.0), |s| {
        let (a, b) = s.split_once(',').expect("offset,sigma");
        (a.parse().expect("cents"), b.parse().expect("cents"))
    });
    let seeds: u32 = std::env::var("EXCITE_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let results: Vec<_> = std::thread::scope(|sc| {
        let hs: Vec<_> = CHORDS
            .iter()
            .enumerate()
            .map(|(ci, &chord)| {
                sc.spawn(move || {
                    let mut w = CombWitness::new(cfg);
                    let mut right = Tally::default();
                    let mut by_kind: Vec<(&'static str, Tally)> = ["missing", "fret", "extra", "neighbour", "muted", "miss-unobs"]
                        .into_iter()
                        .map(|k| (k, Tally::default()))
                        .collect();
                    let mut worst_naming = f32::NEG_INFINITY;
                    let mut lines = Vec::new();
                    let mut sweeps = 0u64;
                    // (offset error against the mean cents of the strings, sharpness, ok)
                    let mut tune: Vec<(f32, f32, bool)> = Vec::new();
                    for seed in 1..=seeds {
                        for mut base in conditions(chord, ci, seed) {
                            if d_off != 0.0 || d_sig != 0.0 {
                                base.cents = detune(d_off, d_sig, base.seed);
                            }
                            let set = set_for(chord, base.dir, base.prev, Some(next_of(ci)));
                            let a = run(&mut w, &base, &set);
                            right.add(&a, None);
                            let mean = base.cents.iter().sum::<f64>() / 6.0;
                            tune.push((a.offset_cents - mean as f32, a.tune_sharp, a.tune_ok));
                            sweeps += u64::from(a.sweeps);
                            let top = a.score[..a.n_alts].iter().copied().fold(f32::NEG_INFINITY, f32::max);
                            worst_naming = worst_naming.max(top);
                            if let Opinion::Names { alts, margin } = a.opinion {
                                let which: Vec<_> = (0..a.n_alts)
                                    .filter(|i| alts & (1 << i) != 0)
                                    .map(|i| format!("{:?}", set.alts[i].kind))
                                    .collect();
                                lines.push(format!("FALSE NAME {base:?} -> {which:?} margin {margin:.1} snr {:.1} expl {:.3}", a.snr_db, a.explained));
                            }
                            // one error of each kind under the same condition
                            for (i, h) in set.alts.iter().enumerate() {
                                let (kind, hand) = match &h.kind {
                                    HypKind::Missing { string } => (0, Hand::Missing(*string as usize)),
                                    HypKind::WrongFret { string, fret } => (1, Hand::Fret(*string as usize, *fret as i8)),
                                    HypKind::Extra { string } => (2, Hand::Extra(*string as usize)),
                                    HypKind::Neighbour { chord } => (
                                        3,
                                        Hand::Chord(CHORDS.iter().copied().find(|c| c == chord).unwrap()),
                                    ),
                                    HypKind::AllMuted => (4, Hand::Muted),
                                    HypKind::Expected => continue,
                                };
                                // thin the error grid: one alternative in three per condition
                                if !(i as u32 + base.seed).is_multiple_of(3) {
                                    continue;
                                }
                                let sc = Scene { hand, ..base };
                                let a = run(&mut w, &sc, &set);
                                let kind = match h.kind {
                                    HypKind::Missing { string }
                                        if a.strings[string as usize].is_none_or(|st| st.obs < cfg.obs_min) =>
                                    {
                                        5
                                    }
                                    _ => kind,
                                };
                                let t = truth(&set, &sc);
                                if kind == 3
                                    && let Opinion::Names { alts, .. } = a.opinion
                                    && t.is_none_or(|t| alts & (1 << t) == 0)
                                {
                                    let which: Vec<_> = (0..a.n_alts)
                                        .filter(|i| alts & (1 << i) != 0)
                                        .map(|i| format!("{:?}", set.alts[i].kind))
                                        .collect();
                                    lines.push(format!("NEIGHBOUR {chord} played {:?} -> {which:?}", h.kind));
                                }
                                by_kind[kind].1.add(&a, t);
                            }
                        }
                    }
                    (chord, right, by_kind, worst_naming, lines, sweeps, tune)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    println!("config: {cfg:?}");
    let mut all = Tally::default();
    let mut kinds: Vec<(&str, Tally)> = Vec::new();
    let mut tune: Vec<(f32, f32, bool)> = Vec::new();
    for (chord, right, by_kind, worst, lines, sweeps, t) in &results {
        tune.extend(t);
        println!(
            "right {chord:3} {}  top naming {worst:6.1}  sweeps/event {:.0}",
            right.line(),
            *sweeps as f64 / right.n as f64
        );
        for l in lines {
            println!("   {l}");
        }
        all.merge(right);
        for (k, t) in by_kind {
            match kinds.iter_mut().find(|(n, _)| n == k) {
                Some((_, acc)) => acc.merge(t),
                None => kinds.push((k, t.clone())),
            }
        }
    }
    println!("right ALL {}", all.line());
    let q = |mut v: Vec<f32>| {
        v.sort_by(f32::total_cmp);
        let n = v.len();
        format!(
            "min {:.1} p5 {:.1} p50 {:.1} p95 {:.1} max {:.1}",
            v[0],
            v[n / 20],
            v[n / 2],
            v[n * 19 / 20],
            v[n - 1]
        )
    };
    if !tune.is_empty() {
        println!(
            "offset error (cents): {}",
            q(tune.iter().map(|t| t.0).filter(|v| v.is_finite()).collect())
        );
        println!(
            "tune sharpness: {}",
            q(tune.iter().map(|t| t.1).filter(|v| v.is_finite()).collect())
        );
        println!(
            "tune_ok {} of {}",
            tune.iter().filter(|t| t.2).count(),
            tune.len()
        );
    }
    for (k, t) in &kinds {
        println!("{k:9} {}", t.line());
    }
}

#[test]
#[ignore = "diagnostic"]
fn dev_strings() {
    let cfg = cfg_from_env();
    let mut w = CombWitness::new(cfg);
    let laptop = std::env::var("EXCITE_LAPTOP").is_ok();
    for (ci, &chord) in CHORDS.iter().enumerate() {
        let set = set_for(chord, Dir::Down, None, Some(next_of(ci)));
        println!("== {chord} {:?}", voicing(chord).0);
        for s in 0..6 {
            let Ask::Must(_) = set.ask[s] else { continue };
            let (mut right, mut miss, mut obs) = (vec![], vec![], vec![]);
            for seed in 1..=40u32 {
                let mut sc = Scene::clean(chord, seed);
                sc.laptop = laptop;
                let a = run(&mut w, &sc, &set);
                let st = a.strings[s].unwrap();
                right.push(st.g_exp);
                obs.push(st.obs);
                sc.hand = Hand::Missing(s);
                let a = run(&mut w, &sc, &set);
                miss.push(a.strings[s].unwrap().g_exp);
            }
            let q = |v: &mut Vec<f32>| {
                v.sort_by(f32::total_cmp);
                (
                    v[0],
                    v[v.len() / 10],
                    v[v.len() / 2],
                    v[v.len() * 9 / 10],
                    v[v.len() - 1],
                )
            };
            println!(
                "  s{s} obs {:?}\n      right g_exp {:?}\n      miss  g_exp {:?}",
                q(&mut obs),
                q(&mut right),
                q(&mut miss)
            );
        }
        // alternative atoms on right strums
        let mut veto = vec![];
        let mut galt = [vec![], vec![], vec![], vec![], vec![], vec![]];
        for seed in 1..=40u32 {
            let mut sc = Scene::clean(chord, seed);
            sc.laptop = laptop;
            let a = run(&mut w, &sc, &set);
            veto.push(a.veto);
            for (v, st) in galt.iter_mut().zip(a.strings) {
                if let Some(st) = st {
                    v.push(st.g_alt);
                }
            }
        }
        veto.sort_by(f32::total_cmp);
        println!(
            "  veto (g/2-lam) min/med/p90/max {:.1} {:.1} {:.1} {:.1}",
            veto[0], veto[20], veto[36], veto[39]
        );
        for (s, v) in galt.iter_mut().enumerate() {
            if !v.is_empty() {
                v.sort_by(f32::total_cmp);
                println!(
                    "  s{s} g_alt med/p90/max {:.1} {:.1} {:.1}",
                    v[v.len() / 2],
                    v[v.len() * 9 / 10],
                    v[v.len() - 1]
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Unit V2 tests
// ---------------------------------------------------------------------------------------

/// `g_exp` a `Must` string needs for `Supports`: it must earn `λ + support_margin` nats.
fn present_level(c: &ExciteConfig) -> f32 {
    2.0 * (c.atom_penalty + c.support_margin)
}

/// `g_exp` below which an atom does not earn its `λ`: the string does not count as sounding.
fn absent_level(c: &ExciteConfig) -> f32 {
    2.0 * c.atom_penalty
}

fn seeds(n: u32, offset: u32) -> impl Iterator<Item = u32> {
    let s = DEV_SEEDS.start + offset;
    (s..s + n).inspect(|s| assert!(DEV_SEEDS.contains(s)))
}

fn alt_index(set: &ConfusionSet, kind: &HypKind) -> usize {
    set.alts
        .iter()
        .position(|h| &h.kind == kind)
        .expect("alternative in the set")
}

/// (1) Right strum, 40 dB, dry: every observable `Must` string earns its place, on all nine
/// chords.
#[test]
fn right_strum_every_observable_string_present() {
    let cfg = ExciteConfig::default();
    let mut w = CombWitness::new(cfg);
    let (mut n, mut supports, mut worst) = (0, 0, f32::INFINITY);
    for (ci, &chord) in CHORDS.iter().enumerate() {
        let set = set_for(chord, Dir::Down, None, Some(next_of(ci)));
        for seed in seeds(8, 100) {
            let a = run(&mut w, &Scene::clean(chord, seed), &set);
            n += 1;
            supports += usize::from(matches!(a.opinion, Opinion::Supports { .. }));
            assert!(
                !matches!(a.opinion, Opinion::Names { .. }),
                "{chord} seed {seed}: {:?}",
                a.opinion
            );
            let mut seen = 0;
            for s in 0..6 {
                if !matches!(set.ask[s], Ask::Must(_)) {
                    continue;
                }
                let st = a.strings[s].expect("stats for a Must string");
                if st.obs < cfg.obs_min {
                    continue;
                }
                seen += 1;
                worst = worst.min(st.g_exp);
                assert!(
                    st.g_exp >= present_level(&cfg),
                    "{chord} seed {seed} string {s}: g_exp {} obs {}",
                    st.g_exp,
                    st.obs
                );
            }
            assert!(seen > 0, "{chord}: no observable string");
        }
    }
    println!("right strums 40 dB dry: {supports}/{n} Supports, lowest observable g_exp {worst:.0}");
    assert!(supports * 10 >= n * 9, "Supports on {supports}/{n}");
}

/// (2) A string the hand misses: where it is observable, `g_exp` falls below the level at
/// which `Missing` is named.
#[test]
fn missing_string_loses_its_evidence() {
    let cfg = ExciteConfig::default();
    let mut w = CombWitness::new(cfg);
    let (mut n, mut named, mut supports, mut worst) = (0, 0, 0, f32::NEG_INFINITY);
    let (mut n_clear, mut named_clear) = (0, 0);
    for (ci, &chord) in CHORDS.iter().enumerate() {
        let set = set_for(chord, Dir::Down, None, Some(next_of(ci)));
        for s in 0..6 {
            let Ask::Must(_) = set.ask[s] else { continue };
            for seed in seeds(4, 200) {
                let sc = Scene {
                    hand: Hand::Missing(s),
                    ..Scene::clean(chord, seed)
                };
                let a = run(&mut w, &sc, &set);
                let st = a.strings[s].expect("stats for a Must string");
                if st.obs < cfg.obs_min {
                    continue;
                }
                n += 1;
                worst = worst.max(st.g_exp);
                assert!(
                    st.g_exp < absent_level(&cfg),
                    "{chord} string {s} seed {seed}: g_exp {} obs {}",
                    st.g_exp,
                    st.obs
                );
                supports += usize::from(matches!(a.opinion, Opinion::Supports { .. }));
                let clear = st.clear >= cfg.obs_min;
                n_clear += usize::from(clear);
                if let Opinion::Names { alts, .. } = a.opinion {
                    let t = truth(&set, &sc).unwrap();
                    assert!(alts & (1 << t) != 0, "{chord} string {s}: named {alts:b}");
                    named += 1;
                    named_clear += usize::from(clear);
                } else if clear {
                    println!(
                        "  not named: {chord} string {s} seed {seed}: {:?} {st:?}",
                        a.opinion
                    );
                }
            }
        }
    }
    println!(
        "observable missing strings: {n}, highest g_exp {worst:.1}, Supports {supports}, named {named}; \
         clear (absence provable) {n_clear}, named {named_clear}"
    );
    assert_eq!(supports, 0, "a missing observable string was supported");
    assert!(
        named_clear * 10 >= n_clear * 9,
        "Missing named on {named_clear}/{n_clear} clear strings"
    );
}

/// (3) An octave-doubled string (the first string of E major is E3 an octave up) has no
/// partial of its own: it is reported unobservable and never named missing.
#[test]
fn octave_doubled_string_is_unobservable() {
    let cfg = ExciteConfig::default();
    let mut w = CombWitness::new(cfg);
    let set = set_for("E", Dir::Down, None, Some("A"));
    let mut opinions = Vec::new();
    for hand in [Hand::Right, Hand::Missing(5)] {
        for seed in seeds(12, 300) {
            let sc = Scene {
                hand,
                ..Scene::clean("E", seed)
            };
            let a = run(&mut w, &sc, &set);
            let st = a.strings[5].unwrap();
            assert!(st.obs < 1e-3 && st.clear < 1e-3, "{st:?}");
            assert!(
                !matches!(a.opinion, Opinion::Names { .. }),
                "{hand:?} seed {seed}: {:?}",
                a.opinion
            );
            opinions.push((hand, a.opinion));
        }
    }
    // An E major with only its first string (E4) left sounding: nothing but that string.
    let lone = Hypothesis {
        kind: HypKind::Expected,
        sounding: [-1, -1, -1, -1, -1, 0],
    };
    let only = ConfusionSet {
        chord: "E4".into(),
        ask: [
            Ask::May,
            Ask::May,
            Ask::May,
            Ask::May,
            Ask::May,
            Ask::Must(0),
        ],
        expected: lone,
        alts: vec![],
        tuning: OPEN,
        capo: 0,
    };
    let a = run(&mut w, &Scene::clean("E", 301), &only);
    assert!(
        a.strings[5].unwrap().obs > 0.99,
        "alone, the string can be seen"
    );
    let missing = opinions.iter().filter(|(h, _)| *h == Hand::Missing(5));
    let sup = missing
        .clone()
        .filter(|(_, o)| matches!(o, Opinion::Supports { .. }))
        .count();
    println!(
        "E with the first string missing: {sup}/{} Supports (Supports says nothing about an unseen string)",
        missing.count()
    );
}

/// (4) The chord before still ringing on strings the new chord does not strike: the ringing
/// floor absorbs it and no `Extra` is named. With the floor switched off (`ring_gain = 0`)
/// the same takes do name `Extra`, so the test has teeth.
#[test]
fn ringing_previous_chord_is_not_an_extra_string() {
    let cases = [
        ("D", "E"),
        ("D", "Em"),
        ("D", "A"),
        ("Dm", "Em"),
        ("F", "E"),
        ("C", "Em"),
        ("Dm", "A"),
    ];
    let mut on = CombWitness::default();
    let mut off = CombWitness::new(ExciteConfig {
        ring_gain: 0.0,
        ..ExciteConfig::default()
    });
    let (mut n, mut extra_off, mut supports) = (0, 0, 0);
    for (chord, prev) in cases {
        let set = set_for(chord, Dir::Down, Some(prev), None);
        for seed in seeds(6, 400) {
            let sc = Scene {
                prev: Some(prev),
                gap: 0.5,
                ..Scene::clean(chord, seed)
            };
            let take = render_scene(&sc);
            n += 1;
            let a = *on.analyze(&take.window(), &set);
            assert!(
                !matches!(a.opinion, Opinion::Names { .. }),
                "{prev}->{chord} seed {seed}: {:?}",
                a.opinion
            );
            supports += usize::from(matches!(a.opinion, Opinion::Supports { .. }));
            if let Opinion::Names { alts, .. } = off.analyze(&take.window(), &set).opinion {
                let extra = (0..set.alts.len()).any(|i| {
                    alts & (1 << i) != 0 && matches!(set.alts[i].kind, HypKind::Extra { .. })
                });
                extra_off += usize::from(extra);
            }
        }
    }
    println!(
        "ringing chord before: {supports}/{n} Supports; without the floor {extra_off}/{n} false Extra"
    );
    assert!(
        extra_off * 2 >= n,
        "control: without the floor only {extra_off}/{n} false Extra"
    );
}

/// (5) The lead of the right alternative grows with the gain of the planted wrong string.
#[test]
fn margin_grows_with_the_planted_string() {
    let gains = [0.02, 0.05, 0.1, 0.2, 0.4, 0.8];
    let mut w = CombWitness::default();
    // (chord, string, fret, alternative)
    let cases = [
        ("D", 0usize, 0i8, HypKind::Extra { string: 0 }),
        ("D", 1, 0, HypKind::Extra { string: 1 }),
        ("C", 0, 0, HypKind::Extra { string: 0 }),
        ("Am", 0, 0, HypKind::Extra { string: 0 }),
    ];
    for (chord, s, fret, kind) in cases {
        let set = set_for(chord, Dir::Down, None, None);
        let i = alt_index(&set, &kind);
        for seed in seeds(3, 500) {
            let mut prev = f32::NEG_INFINITY;
            let mut line = format!("{chord} +{kind:?} seed {seed}:");
            for g in gains {
                let sc = Scene {
                    planted: Some((s, fret, g)),
                    ..Scene::clean(chord, seed)
                };
                let a = run(&mut w, &sc, &set);
                let score = a.score[i];
                line += &format!(" {score:.0}");
                assert!(score >= prev - 1.0, "{line}: not monotone");
                prev = score;
                if g >= 0.4 {
                    match a.opinion {
                        Opinion::Names { alts, margin } => {
                            assert!(
                                alts & (1 << i) != 0 && margin >= score - 1e-3,
                                "{line}: {alts:b}"
                            );
                        }
                        o => panic!("{line}: {o:?} at gain {g}"),
                    }
                }
            }
            println!("{line}");
        }
    }
}

/// (6) Silent, clipped, non-finite, too short and malformed input: an abstention with the
/// right reason, never a panic.
#[test]
fn bad_input_abstains_with_the_right_reason() {
    let mut w = CombWitness::default();
    let set = set_for("G", Dir::Down, None, Some("C"));
    let take = render_scene(&Scene::clean("G", 600));
    let good = take.window();
    let check =
        |w: &mut CombWitness, win: &Window, set: &ConfusionSet, fault: Fault, why: Abstain| {
            let a = *w.analyze(win, set);
            assert_eq!(a.fault, Some(fault), "{:?}", a.opinion);
            assert_eq!(a.opinion, Opinion::Abstains(why));
            assert_eq!(w.opine(win, set), Opinion::Abstains(why));
        };
    let zeros = vec![0.0f32; 20_000];
    let silent = Window {
        fs: SR,
        pre: &zeros[..8000],
        post: &zeros,
    };
    check(&mut w, &silent, &set, Fault::Silent, Abstain::LowSnr);
    // the same strum, before and after: nothing new
    let same = Window {
        fs: SR,
        pre: good.post,
        post: good.post,
    };
    assert_eq!(w.opine(&same, &set), Opinion::Abstains(Abstain::LowSnr));

    let mut clipped = good.post.to_vec();
    clipped
        .iter_mut()
        .for_each(|v| *v = (*v * 100.0).clamp(-1.0, 1.0));
    let win = Window {
        post: &clipped,
        ..good
    };
    check(&mut w, &win, &set, Fault::Clipped, Abstain::Unavailable);

    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut x = good.post.to_vec();
        x[777] = bad;
        let win = Window { post: &x, ..good };
        check(&mut w, &win, &set, Fault::NonFinite, Abstain::Unavailable);
        let mut p = good.pre.to_vec();
        p[3] = bad;
        let win = Window { pre: &p, ..good };
        check(&mut w, &win, &set, Fault::NonFinite, Abstain::Unavailable);
    }

    let short = Window {
        post: &good.post[..2000],
        ..good
    };
    check(&mut w, &short, &set, Fault::PostShort, Abstain::TooShort);
    let short = Window {
        pre: &good.pre[good.pre.len() - 500..],
        ..good
    };
    check(&mut w, &short, &set, Fault::PreShort, Abstain::TooShort);
    let empty = Window {
        fs: SR,
        pre: &[],
        post: &[],
    };
    check(&mut w, &empty, &set, Fault::PostShort, Abstain::TooShort);
    // a stroke too close to the end of the recording: `cut` leaves a short `post`
    let end = take.attack + 1000;
    let tight = cut(&take.audio[..end], SR, take.attack, take.last, 0, end);
    check(&mut w, &tight, &set, Fault::PostShort, Abstain::TooShort);

    for fs in [f64::NAN, f64::INFINITY, 0.0, -48_000.0, 8000.0, 1e6] {
        let win = Window { fs, ..good };
        check(&mut w, &win, &set, Fault::BadRate, Abstain::Unavailable);
    }

    let mut bad = set.clone();
    bad.expected.sounding[2] = 99;
    check(&mut w, &good, &bad, Fault::BadSet, Abstain::Unavailable);
    let mut bad = set.clone();
    bad.alts[0].sounding[0] = -3;
    check(&mut w, &good, &bad, Fault::BadSet, Abstain::Unavailable);
    let mut bad = set.clone();
    bad.expected.sounding = [-1; 6];
    check(&mut w, &good, &bad, Fault::BadSet, Abstain::Unavailable);
    let mut bad = set.clone();
    while bad.alts.len() <= MAX_ALTS {
        bad.alts.push(set.alts[0].clone());
    }
    check(&mut w, &good, &bad, Fault::BadSet, Abstain::Unavailable);
    let mut bad = set.clone();
    bad.tuning = [i32::MAX; 6];
    check(&mut w, &good, &bad, Fault::BadSet, Abstain::Unavailable);

    // a set the witness can read still works afterwards
    assert!(matches!(w.opine(&good, &set), Opinion::Supports { .. }));
}

/// (7) Negative control: a dictionary one semitone off never supports a right strum.
#[test]
fn shifted_dictionary_never_supports() {
    let mut w = CombWitness::new(ExciteConfig {
        dict_shift: 1,
        ..ExciteConfig::default()
    });
    let mut right = CombWitness::default();
    let (mut n, mut shifted, mut base) = (0, 0, 0);
    for (ci, &chord) in CHORDS.iter().enumerate() {
        let set = set_for(chord, Dir::Down, None, Some(next_of(ci)));
        for seed in seeds(6, 700) {
            let take = render_scene(&Scene::clean(chord, seed));
            n += 1;
            let o = w.opine(&take.window(), &set);
            shifted += usize::from(matches!(o, Opinion::Supports { .. }));
            base += usize::from(matches!(
                right.opine(&take.window(), &set),
                Opinion::Supports { .. }
            ));
        }
    }
    println!(
        "Supports on right strums: {base}/{n} with the true dictionary, {shifted}/{n} shifted"
    );
    assert_eq!(shifted, 0);
    assert!(base * 10 >= n * 9);
}

/// (9) A detuned guitar, right strum, 40 dB, dry: the witness still supports the chord and
/// never names an error. Whole guitar ±15 and ±30 cents, and each string off by σ 5 cents.
#[test]
fn detuned_right_strum_supports() {
    let mut w = CombWitness::default();
    for (offset, sigma) in [
        (-15.0, 0.0),
        (15.0, 0.0),
        (-30.0, 0.0),
        (30.0, 0.0),
        (0.0, 5.0),
    ] {
        let (mut n, mut supports) = (0, 0);
        for (ci, &chord) in CHORDS.iter().enumerate() {
            let set = set_for(chord, Dir::Down, None, Some(next_of(ci)));
            for seed in seeds(8, 1100) {
                let sc = Scene {
                    cents: detune(offset, sigma, seed),
                    ..Scene::clean(chord, seed)
                };
                let a = run(&mut w, &sc, &set);
                n += 1;
                supports += usize::from(matches!(a.opinion, Opinion::Supports { .. }));
                assert!(
                    !matches!(a.opinion, Opinion::Names { .. }),
                    "{chord} seed {seed} offset {offset} sigma {sigma}: {:?}",
                    a.opinion
                );
            }
        }
        println!("detune {offset:+} cents, sigma {sigma}: {supports}/{n} Supports, 0 Names");
        assert!(
            supports * 10 >= n * 7,
            "offset {offset} sigma {sigma}: Supports on {supports}/{n}"
        );
    }
}

/// (10) The guitar-wide offset is read to within 3 cents, from -30 to +30 cents.
#[test]
fn offset_estimate_accuracy() {
    let mut w = CombWitness::default();
    let mut worst = 0.0f32;
    for offset in [-30.0, -15.0, 0.0, 15.0, 30.0] {
        for (ci, &chord) in CHORDS.iter().enumerate() {
            let set = set_for(chord, Dir::Down, None, Some(next_of(ci)));
            for seed in seeds(3, 1200) {
                let sc = Scene {
                    cents: [offset; 6],
                    ..Scene::clean(chord, seed)
                };
                let a = run(&mut w, &sc, &set);
                let err = a.offset_cents - offset as f32;
                assert!(
                    err.abs() <= 3.0,
                    "{chord} seed {seed} offset {offset}: read {}",
                    a.offset_cents
                );
                worst = worst.max(err.abs());
            }
        }
    }
    println!("offset estimate: worst error {worst:.2} cents");
}

/// (11) A guitar ±60 cents off is out of range: the witness abstains as unavailable
/// (`Fault::Detuned`) and never names.
#[test]
fn far_detune_abstains() {
    let mut w = CombWitness::default();
    for offset in [-60.0, 60.0] {
        for (ci, &chord) in CHORDS.iter().enumerate() {
            let set = set_for(chord, Dir::Down, None, Some(next_of(ci)));
            for seed in seeds(2, 1300) {
                for hand in [Hand::Right, Hand::Fret(2, 4)] {
                    let sc = Scene {
                        hand,
                        cents: [offset; 6],
                        ..Scene::clean(chord, seed)
                    };
                    let a = run(&mut w, &sc, &set);
                    assert_eq!(
                        a.fault,
                        Some(Fault::Detuned),
                        "{chord} seed {seed} offset {offset}: {:?}",
                        a.opinion
                    );
                    assert_eq!(a.opinion, Opinion::Abstains(Abstain::Unavailable));
                }
            }
        }
    }
}

/// (8) Cost per event, measured and reported (reference: under 10 ms in release on the Mac;
/// not a gate). The grid mixes ringing chords, reverb, laptop mic and both directions, in
/// tune and detuned (±25 cents, σ 5 cents per string).
#[test]
fn cost_per_event() {
    let mut w = CombWitness::default();
    let mut times = Vec::new();
    let mut sweeps = 0u64;
    for (ci, &chord) in CHORDS.iter().enumerate() {
        let detuned = conditions(chord, ci, 9)
            .into_iter()
            .step_by(8)
            .map(|sc| Scene {
                cents: detune(if sc.seed % 2 == 0 { 25.0 } else { -25.0 }, 5.0, sc.seed),
                ..sc
            });
        for sc in conditions(chord, ci, 9)
            .into_iter()
            .step_by(8)
            .chain(detuned)
        {
            let set = set_for(chord, sc.dir, sc.prev, Some(next_of(ci)));
            let take = render_scene(&sc);
            let win = take.window();
            let t = Instant::now();
            let a = w.analyze(&win, &set);
            times.push(t.elapsed().as_secs_f64() * 1e3);
            sweeps += u64::from(a.sweeps);
            assert!(!a.capped, "{sc:?}");
        }
    }
    times.sort_by(f64::total_cmp);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "cost per event over {} events ({}): mean {mean:.2} ms, median {:.2} ms, max {:.2} ms; {:.0} sweeps/event",
        times.len(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        times[times.len() / 2],
        times[times.len() - 1],
        sweeps as f64 / times.len() as f64
    );
}

/// Same input, same analysis, bit for bit, whatever came before.
#[test]
fn deterministic() {
    let set = set_for("C", Dir::Up, Some("G"), Some("Am"));
    let sc = Scene {
        prev: Some("G"),
        dir: Dir::Up,
        wet: 0.25,
        laptop: true,
        snr_db: 20.0,
        ..Scene::clean("C", 800)
    };
    let take = render_scene(&sc);
    let mut a = CombWitness::default();
    let mut b = CombWitness::default();
    let first = *a.analyze(&take.window(), &set);
    // a different event in between
    let other = render_scene(&Scene::clean("E", 801));
    b.analyze(&other.window(), &set_for("E", Dir::Down, None, None));
    let second = *b.analyze(&take.window(), &set);
    assert_eq!(format!("{first:?}"), format!("{second:?}"));
    assert_eq!(a.family(), Family::Dsp);
    assert_eq!(a.name(), "comb");
    assert_eq!(a.stats(&take.window(), &set), first.strings);
}

/// No allocation per event once the witness exists (debug and release).
#[test]
fn no_allocation_per_event() {
    let cases: Vec<(Take, ConfusionSet)> = [
        ("G", None, 0.0),
        ("D", Some("E"), 0.0),
        ("F", Some("F"), 0.0),
        ("C", None, -25.0),
        ("Am", Some("G"), 20.0),
        ("E", None, 60.0),
    ]
    .into_iter()
    .enumerate()
    .map(|(k, (chord, prev, offset))| {
        let sc = Scene {
            prev,
            cents: detune(
                offset,
                if offset == 0.0 { 0.0 } else { 5.0 },
                900 + k as u32,
            ),
            hand: if k == 1 {
                Hand::Fret(4, 2)
            } else {
                Hand::Right
            },
            ..Scene::clean(chord, 900 + k as u32)
        };
        (
            render_scene(&sc),
            set_for(chord, Dir::Down, prev, Some("C")),
        )
    })
    .collect();
    let zeros = vec![0.0f32; 20_000];
    let mut w = CombWitness::default();
    let before = allocs();
    for (take, set) in &cases {
        w.opine(&take.window(), set);
        w.opine(
            &Window {
                fs: SR,
                pre: &zeros,
                post: &zeros,
            },
            set,
        );
        w.opine(
            &Window {
                fs: 96_000.0,
                ..take.window()
            },
            set,
        );
        w.opine(
            &Window {
                fs: 22_050.0,
                ..take.window()
            },
            set,
        );
    }
    assert_eq!(allocs() - before, 0, "allocations per event");
}

/// `cut` keeps the guards of plan section 2.4 and never crosses the neighbouring strokes.
#[test]
fn cut_keeps_the_guards() {
    let audio = vec![0.0f32; 48_000];
    let w = cut(&audio, SR, 20_000, 21_000, 15_000, 30_000);
    let off = |x: &[f32]| x.as_ptr() as usize - audio.as_ptr() as usize;
    let pre_end = (off(w.pre) / 4) + w.pre.len();
    assert_eq!(pre_end, 20_000 - 240, "pre ends 5 ms before the attack");
    assert_eq!(off(w.pre) / 4, 15_000, "pre starts after the stroke before");
    assert_eq!(
        off(w.post) / 4,
        21_000 + 480,
        "post starts 10 ms after the last string"
    );
    assert_eq!(
        w.post.len(),
        8192,
        "post is at most one frame (4096 samples after decimation)"
    );
    let w = cut(&audio, SR, 20_000, 21_000, 15_000, 25_000);
    assert_eq!(
        off(w.post) / 4 + w.post.len(),
        25_000 - 240,
        "post stops before the next stroke"
    );
    let w = cut(&audio, SR, 100, 50, 0, 48_000);
    assert!(w.pre.is_empty());
    assert_eq!(w.post.len(), 8192, "post is one frame long");
    let w = cut(&audio, f64::NAN, 100, 200, 0, 48_000);
    assert!(w.post.is_empty() && w.pre.is_empty());
}
