//! Synthetic bench of the chord verifier (the verifier/learner design plan, unit V4).
//!
//! Grid: 9 chords in 3 progressions (so every chord has neighbours) x {right; each Missing,
//! WrongFret, Extra and Neighbour alternative of the closed set; muted; two wrong frets at once
//! (out of the set)} x SNR {40, 20 dB} x {dry, reverberated} x {flat, laptop microphone} x
//! {down, up} x spread {15, 60 ms} x seeds. The witness is the DSP `CombWitness` at its frozen
//! operating point; the second witness is assumed to agree in every case (worst case).
//!
//! Seeds are at or above `RESERVED_SEEDS` (the witness was tuned on `DEV_SEEDS`, 0..10000), and
//! one acoustic condition (30 dB, 15% wet, 35 ms spread) was never in the development grid.
//!
//! The long run is `#[ignore]`:
//! `cargo test -p guitarness-verify --release --test bench_synth -- --ignored --nocapture`.
//! Set `BENCH_TABLE=<path>` to write the per-pair table there. A small subset (a few seconds)
//! always runs. Nothing here unlocks a gate: the bench only reports.

use std::sync::{Arc, OnceLock};

use guitarness_core::chart::{
    Chart, Dir as CDir, Event, EventKind, Loc, Style, Voicing as CVoicing,
};
use guitarness_core::gates::GateTable;
use guitarness_core::verdict::VerdictType;
use guitarness_dsp::synth::Mulberry32;
use guitarness_sim::fx::{hum, laptop_mic, pink, reverb, rms};
use guitarness_sim::strum::{Dir, StrumSpec, Voicing, render_take, render_take_with, voicing};
use guitarness_verify::bench::{
    BenchReport, Class, Cond, Scenario, Tag, Take, Truth, frac, label_of, open_gates, run, wilson,
    worst_case_cfg,
};
use guitarness_verify::excite::{CombWitness, DEV_SEEDS, ExciteConfig};
use guitarness_verify::hyp::confusion_set;
use guitarness_verify::types::{Abstain, Ask, ConfusionSet, HypKind, Opinion};

const SR: f64 = 48_000.0;
/// First seed of the bench. Everything below `DEV_SEEDS.end` was used to choose the operating point.
const RESERVED_SEEDS: u32 = 1_000_000;
/// Level of the judged strum (RMS of its first 50 ms), as in `guitarness_sim::timing`.
const STRUM_RMS: f64 = 0.1;

const PROGRESSIONS: [&[&str]; 3] = [
    &["G", "Em", "C", "D"],
    &["A", "E", "A", "D", "E"],
    &["Am", "F", "C", "Dm", "Am", "G"],
];

// ---------------------------------------------------------------------------------------
// Rendering

/// What the hand really does.
#[derive(Clone, Copy, Debug)]
enum Hand {
    Right,
    Missing(usize),
    Fret(usize, i8),
    Extra(usize),
    Chord(&'static str),
    Muted,
    /// Two strings at wrong frets at once: outside any closed set.
    Two((usize, i8), (usize, i8)),
}

/// Tuning error of the guitar. The default is the exact 440 Hz of the dictionary.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Detune {
    None,
    /// Every string off by the same number of cents.
    Offset(f64),
    /// Every string off by its own Gaussian draw, standard deviation in cents.
    Sigma(f64),
    /// The whole guitar off by `.0` cents, every string by its own draw of `.1` around it.
    Both(f64, f64),
}

#[derive(Clone, Copy, Debug)]
struct Scene {
    chord: &'static str,
    prev: Option<&'static str>,
    prev_gain: f64,
    gap: f64,
    hand: Hand,
    cond: Cond,
    seed: u32,
    detune: Detune,
}

fn render_scene(sc: &Scene) -> Take {
    let mut rng = Mulberry32::new(sc.seed.wrapping_mul(2_654_435_761).wrapping_add(17));
    let (lo, hi) = if sc.cond.down { (0, 5) } else { (2, 5) };
    let dir = if sc.cond.down { Dir::Down } else { Dir::Up };
    let spread = f64::from(sc.cond.spread_ms) * 1e-3;
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
        Hand::Two((a, fa), (b, fb)) => {
            frets[a] = fa;
            frets[b] = fb;
        }
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
        dir,
        spread,
        gain: 1.0,
        muted,
        strings,
        attack: 0.001,
    });
    let len = ((t0 + spread + 0.30) * SR) as usize;
    let (r_audio, string_times) = if sc.detune == Detune::None {
        let r = render_take(SR, &specs, len, &mut rng);
        (r.audio, r.string_times)
    } else {
        render_detuned(&specs, len, &mut rng, sc)
    };
    let mut audio = r_audio;
    let attack = (t0 * SR).round() as usize;
    let main = usize::from(sc.prev.is_some());
    let last = string_times
        .get(main)
        .and_then(|t| t.iter().copied().reduce(f64::max))
        .map_or(attack, |t| (t * SR).round() as usize);
    let level = rms(&audio[attack..attack + (0.05 * SR) as usize]).max(1e-9);
    audio
        .iter_mut()
        .for_each(|v| *v = (*v as f64 * STRUM_RMS / level) as f32);
    reverb(&mut audio, SR, 0.5, f64::from(sc.cond.wet));
    let noise_rms = STRUM_RMS * 10f64.powf(-f64::from(sc.cond.snr_db) / 20.0);
    let n = pink(len, &mut rng);
    let h = hum(len, SR, 60.0);
    for ((a, nv), hv) in audio.iter_mut().zip(&n).zip(&h) {
        *a += (noise_rms * (*nv as f64 + 0.5 * *hv as f64)) as f32;
    }
    if sc.cond.laptop {
        laptop_mic(&mut audio, SR);
    }
    let prev_end = if sc.prev.is_some() {
        ((t_prev + 0.02 + 0.010) * SR) as usize
    } else {
        0
    };
    Take {
        audio,
        fs: SR,
        attack,
        last,
        prev_end,
    }
}

/// Cents of every string for a detuned scene (the sim tunes 440 Hz exactly and has no pitch
/// parameter, so the detune is applied to each rendered string by resampling).
fn cents_of(sc: &Scene) -> [f64; 6] {
    match sc.detune {
        Detune::None => [0.0; 6],
        Detune::Offset(c) => [c; 6],
        Detune::Sigma(sd) | Detune::Both(_, sd) => {
            let mut r = Mulberry32::new(sc.seed.wrapping_mul(40_503).wrapping_add(0x9E37));
            let mut c = [match sc.detune {
                Detune::Both(o, _) => o,
                _ => 0.0,
            }; 6];
            for v in &mut c {
                // Box-Muller
                let (u1, u2) = (r.next_f64().max(1e-12), r.next_f64());
                *v += sd * (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
            }
            c
        }
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

/// Render as `render_take` (same random draws), then rebuild the mix with each string moved by
/// its own cents. Only for strums without a muted burst.
fn render_detuned(
    specs: &[StrumSpec],
    len: usize,
    rng: &mut Mulberry32,
    sc: &Scene,
) -> (Vec<f32>, Vec<Vec<f64>>) {
    let cents = cents_of(sc);
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

// ---------------------------------------------------------------------------------------
// Charts and sets

struct Pos {
    chord: &'static str,
    /// Chord before it in its progression (cyclic), the one that may still ring.
    prev: &'static str,
    down: Arc<ConfusionSet>,
    up: Arc<ConfusionSet>,
}

fn chart_of(prog: &[&'static str], dir: CDir) -> Chart {
    let mut c = Chart::new("bench", 100.0);
    for name in prog {
        let v = voicing(name).0;
        c.voicings.insert(
            (*name).to_string(),
            CVoicing {
                frets: v,
                req: None,
                fingers: [0; 6],
                root: None,
            },
        );
    }
    for (i, name) in prog.iter().enumerate() {
        c.tiers[0].events.push(Event {
            tick: i as u32 * 480,
            dur: 480,
            kind: EventKind::Strum {
                dir,
                span: None,
                style: Style::Full,
                roll_ms: None,
            },
            chord: Some((*name).to_string()),
        });
    }
    c
}

fn positions() -> Vec<Pos> {
    let mut out = Vec::new();
    for prog in PROGRESSIONS {
        let down = chart_of(prog, CDir::Down);
        let up = chart_of(prog, CDir::Up);
        for (i, &chord) in prog.iter().enumerate() {
            let loc = Loc { tier: 0, index: i };
            out.push(Pos {
                chord,
                prev: prog[(i + prog.len() - 1) % prog.len()],
                down: Arc::new(confusion_set(&down, loc, false).expect("down set")),
                up: Arc::new(confusion_set(&up, loc, false).expect("up set")),
            });
        }
    }
    out
}

fn tonic(c: &str) -> &str {
    c.trim_end_matches('m')
}

fn static_name(c: &str) -> &'static str {
    ["C", "D", "E", "G", "A", "Am", "Em", "Dm", "F"]
        .into_iter()
        .find(|n| *n == c)
        .expect("chord of the nine")
}

// ---------------------------------------------------------------------------------------
// Grid

/// 32 standard conditions (the shape of the development grid).
fn std_conds() -> Vec<Cond> {
    let mut v = Vec::new();
    for down in [true, false] {
        for spread_ms in [15.0, 60.0] {
            for snr_db in [40.0, 20.0] {
                for wet in [0.0, 0.25] {
                    for laptop in [false, true] {
                        v.push(Cond {
                            snr_db,
                            wet,
                            laptop,
                            down,
                            spread_ms,
                        });
                    }
                }
            }
        }
    }
    v
}

/// The condition no development run ever used: 30 dB, 15% wet, 35 ms.
fn reserved_conds() -> Vec<Cond> {
    let mut v = Vec::new();
    for down in [true, false] {
        for laptop in [false, true] {
            v.push(Cond {
                snr_db: 30.0,
                wet: 0.15,
                laptop,
                down,
                spread_ms: 35.0,
            });
        }
    }
    v
}

fn scene_for(pos: &Pos, hand: Hand, cond: Cond, seed: u32) -> Scene {
    let prev = match seed % 3 {
        0 => None,
        1 => Some(pos.chord),
        _ => Some(static_name(pos.prev)),
    };
    Scene {
        chord: pos.chord,
        prev,
        prev_gain: [0.5, 1.0, 2.0][(seed as usize / 3) % 3],
        gap: [0.33, 0.5, 1.0][(seed as usize / 7) % 3],
        hand,
        cond,
        seed,
        detune: Detune::None,
    }
}

fn scenario(
    pos: &Pos,
    set: &Arc<ConfusionSet>,
    class: Class,
    label: String,
    truth: Truth,
    tonic_differs: bool,
    sc: Scene,
) -> Scenario {
    Scenario {
        tag: Tag {
            chord: pos.chord.to_string(),
            class,
            label,
            tonic_differs,
            cond: sc.cond,
            seed: sc.seed,
        },
        set: Arc::clone(set),
        truth,
        render: Box::new(move || render_scene(&sc)),
    }
}

/// Correct strums: every position x `conds`, once per seed index in `seeds`.
fn right_grid(pos: &[&Pos], conds: &[Cond], seeds: std::ops::Range<u32>) -> Vec<Scenario> {
    right_grid_d(pos, conds, seeds, Detune::None, false)
}

/// A bench seed moved into `DEV_SEEDS`, for runs that may be used to choose the operating
/// point (collisions between two bench seeds do not matter there).
fn dev_seed(seed: u32) -> u32 {
    let h = seed.wrapping_mul(2_654_435_761) >> 8;
    DEV_SEEDS.start + h % (DEV_SEEDS.end - DEV_SEEDS.start)
}

/// As `right_grid`, with a detune; `dev` moves every seed into `DEV_SEEDS`.
fn right_grid_d(
    pos: &[&Pos],
    conds: &[Cond],
    seeds: std::ops::Range<u32>,
    detune: Detune,
    dev: bool,
) -> Vec<Scenario> {
    let mut out = Vec::new();
    for sidx in seeds {
        let mut n = 0u32;
        for p in pos {
            for c in conds {
                n += 1;
                let seed = RESERVED_SEEDS + sidx * 100_000 + n;
                let seed = if dev { dev_seed(seed) } else { seed };
                let set = if c.down { &p.down } else { &p.up };
                let sc = Scene {
                    detune,
                    ..scene_for(p, Hand::Right, *c, seed)
                };
                out.push(scenario(
                    p,
                    set,
                    Class::Right,
                    "right".into(),
                    Truth::Right,
                    false,
                    sc,
                ));
            }
        }
    }
    out
}

/// The hand that realises an alternative of the set.
fn hand_of(kind: &HypKind) -> Option<Hand> {
    Some(match kind {
        HypKind::Missing { string } => Hand::Missing(*string as usize),
        HypKind::WrongFret { string, fret } => Hand::Fret(*string as usize, *fret as i8),
        HypKind::Extra { string } => Hand::Extra(*string as usize),
        HypKind::Neighbour { chord } => Hand::Chord(static_name(chord)),
        HypKind::AllMuted => Hand::Muted,
        HypKind::Expected => return None,
    })
}

/// Two wrong frets at once, on the two lowest (`a`) or two highest (`b`) `Must` strings.
fn out_of_set(set: &ConfusionSet) -> Vec<(String, Hand)> {
    let musts: Vec<(usize, u8)> = set
        .ask
        .iter()
        .enumerate()
        .filter_map(|(s, a)| match a {
            Ask::Must(f) => Some((s, *f)),
            _ => None,
        })
        .collect();
    if musts.len() < 2 {
        return Vec::new();
    }
    let bump = |(s, f): (usize, u8)| (s, if f == 0 { 2 } else { f as i8 - 1 });
    let up = |(s, f): (usize, u8)| (s, f as i8 + 1);
    let n = musts.len();
    vec![
        (
            "OutOfSet low2 +1".to_string(),
            Hand::Two(up(musts[0]), up(musts[1])),
        ),
        (
            "OutOfSet high2 -1".to_string(),
            Hand::Two(bump(musts[n - 2]), bump(musts[n - 1])),
        ),
    ]
}

/// Mistakes: every position x both strokes x each alternative of the set (and two out of the
/// set), under the `combos` listed of the 8 (SNR, wet, microphone) combinations. `alt_stride`
/// thins the alternatives; the spread alternates with the alternative, the combination and the
/// seed so both values appear.
fn error_grid(
    pos: &[&Pos],
    seeds: std::ops::Range<u32>,
    combos: &[usize],
    alt_stride: usize,
) -> Vec<Scenario> {
    error_grid_d(pos, seeds, combos, alt_stride, Detune::None, false, false)
}

/// As `error_grid`; `ring_only` keeps the alternatives that ring (no muted strums, no
/// out-of-set pair) so a detune can be applied.
fn error_grid_d(
    pos: &[&Pos],
    seeds: std::ops::Range<u32>,
    combos: &[usize],
    alt_stride: usize,
    detune: Detune,
    only: bool,
    dev: bool,
) -> Vec<Scenario> {
    let combo = |c: usize, down: bool, spread_ms: f32| Cond {
        snr_db: if c & 1 == 0 { 40.0 } else { 20.0 },
        wet: if c & 2 == 0 { 0.0 } else { 0.25 },
        laptop: c & 4 != 0,
        down,
        spread_ms,
    };
    let mut out = Vec::new();
    for sidx in seeds {
        let mut n = 0u32;
        for p in pos {
            for down in [true, false] {
                let set = if down { &p.down } else { &p.up };
                let mut cases: Vec<(Class, String, Truth, bool, Hand)> = Vec::new();
                for (i, h) in set.alts.iter().enumerate() {
                    let Some(hand) = hand_of(&h.kind) else {
                        continue;
                    };
                    let differs = matches!(&h.kind, HypKind::Neighbour { chord } if tonic(chord) != tonic(p.chord));
                    cases.push((
                        Class::of(&h.kind),
                        label_of(&h.kind),
                        Truth::Alt(i),
                        differs,
                        hand,
                    ));
                }
                for (label, hand) in out_of_set(set) {
                    if only {
                        break;
                    }
                    cases.push((Class::OutOfSet, label, Truth::OutOfSet, false, hand));
                }
                for (i, (class, label, truth, differs, hand)) in cases.into_iter().enumerate() {
                    if class != Class::OutOfSet && i % alt_stride != 0 {
                        continue;
                    }
                    for &c in combos {
                        n += 1;
                        let spread = if (i + c + sidx as usize).is_multiple_of(2) {
                            15.0
                        } else {
                            60.0
                        };
                        let seed = RESERVED_SEEDS + sidx * 100_000 + 50_000 + n;
                        let seed = if dev { dev_seed(seed) } else { seed };
                        let sc = Scene {
                            detune,
                            ..scene_for(p, hand, combo(c, down, spread), seed)
                        };
                        out.push(scenario(p, set, class, label.clone(), truth, differs, sc));
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Running

fn run_par(grid: Vec<Scenario>, cfg: ExciteConfig) -> BenchReport {
    let threads = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .min(10);
    let chunk = grid.len().div_ceil(threads).max(1);
    let verify = worst_case_cfg(&cfg);
    let parts: Vec<BenchReport> = std::thread::scope(|s| {
        let hs: Vec<_> = grid
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    let mut w = CombWitness::new(cfg);
                    run(part, &mut w, &verify)
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().expect("bench thread"))
            .collect()
    });
    let mut rep = BenchReport::default();
    for p in parts {
        rep.merge(p);
    }
    rep
}

struct Bench {
    /// Correct strums on the standard conditions.
    right_std: BenchReport,
    /// Correct strums on the reserved condition.
    right_res: BenchReport,
    errors: BenchReport,
    /// Same correct strums, dictionary shifted a semitone (negative control).
    control: BenchReport,
    secs: f64,
}

/// What one bench run covers.
struct Plan<'a> {
    pos: Vec<&'a Pos>,
    std_c: Vec<Cond>,
    res_c: Vec<Cond>,
    std_seeds: std::ops::Range<u32>,
    res_seeds: std::ops::Range<u32>,
    err_seeds: std::ops::Range<u32>,
    combos: Vec<usize>,
    stride: usize,
}

fn build_bench(full: bool) -> Bench {
    let t = std::time::Instant::now();
    let all = positions();
    let cfg = ExciteConfig::default();
    let Plan {
        pos,
        std_c,
        res_c,
        std_seeds,
        res_seeds,
        err_seeds,
        combos,
        stride,
    } = if full {
        Plan {
            pos: all.iter().collect(),
            std_c: std_conds(),
            res_c: reserved_conds(),
            std_seeds: 0..8,
            res_seeds: 8..33,
            err_seeds: 100..103,
            combos: (0..8).collect(),
            stride: 1,
        }
    } else {
        // One position per chord, a handful of conditions, thinned alternatives.
        let mut seen = Vec::new();
        let pos: Vec<&Pos> = all
            .iter()
            .filter(|p| {
                let fresh = !seen.contains(&p.chord);
                seen.push(p.chord);
                fresh
            })
            .collect();
        let sc = std_conds();
        let pick = |i: &[usize]| i.iter().map(|&k| sc[k]).collect::<Vec<_>>();
        Plan {
            pos,
            std_c: pick(&[0, 3, 12, 21, 27, 30, 5, 18]),
            res_c: reserved_conds(),
            std_seeds: 0..4,
            res_seeds: 4..8,
            err_seeds: 100..101,
            combos: vec![0, 3, 4, 6],
            stride: 1,
        }
    };
    let right_std = run_par(right_grid(&pos, &std_c, std_seeds.clone()), cfg);
    let right_res = run_par(right_grid(&pos, &res_c, res_seeds), cfg);
    let errors = run_par(error_grid(&pos, err_seeds, &combos, stride), cfg);
    let shifted = ExciteConfig {
        dict_shift: 1,
        ..cfg
    };
    // The control sees the same correct strums, at least 200 of them.
    let control = run_par(right_grid(&pos, &std_c, 0..2), shifted);
    Bench {
        right_std,
        right_res,
        errors,
        control,
        secs: t.elapsed().as_secs_f64(),
    }
}

fn subset() -> &'static Bench {
    static B: OnceLock<Bench> = OnceLock::new();
    B.get_or_init(|| build_bench(false))
}

fn full() -> &'static Bench {
    static B: OnceLock<Bench> = OnceLock::new();
    B.get_or_init(|| {
        let b = build_bench(true);
        println!("full bench built in {:.1} s", b.secs);
        b
    })
}

fn merged(b: &Bench) -> BenchReport {
    let mut r = BenchReport::default();
    r.merge(b.right_std.clone());
    r.merge(b.right_res.clone());
    r.merge(b.errors.clone());
    r
}

// ---------------------------------------------------------------------------------------
// The four checks

/// `Names` of the witness on correct strums: the false-accusation exposure in the worst case.
fn names_rate(rep: &BenchReport) -> (usize, usize) {
    let t = rep.tally(|r| r.tag.class == Class::Right);
    (t.names(), t.n)
}

fn line(what: &str, k: usize, n: usize) {
    let (lo, hi) = wilson(k, n);
    println!(
        "{what}: Names {k} of {n} correct strums = {:.3}%  Wilson 95% [{:.3}%, {:.3}%]",
        100.0 * frac(k, n),
        100.0 * lo,
        100.0 * hi
    );
}

fn check1(b: &Bench, min_n: usize, tag: &str) {
    let (ks, ns) = names_rate(&b.right_std);
    let (kr, nr) = names_rate(&b.right_res);
    let (k, n) = (ks + kr, ns + nr);
    println!("[{tag}] test 1: reserved seeds >= {RESERVED_SEEDS}, dev seeds were {DEV_SEEDS:?}");
    line(&format!("[{tag}] standard conditions"), ks, ns);
    line(
        &format!("[{tag}] reserved condition (30 dB, 15% wet, 35 ms)"),
        kr,
        nr,
    );
    line(&format!("[{tag}] all"), k, n);
    let all = b.right_std.tally(|r| r.tag.class == Class::Right);
    let res = b.right_res.tally(|r| r.tag.class == Class::Right);
    println!(
        "[{tag}] decide (second witness always agrees): false accusations {} + {}, Ok {} of {} + {} of {}",
        all.wrong_false, res.wrong_false, all.ok, all.n, res.ok, res.n
    );
    assert!(n >= min_n, "{n} correct strums, need {min_n}");
    assert!(
        200 * k <= n,
        "Names on {k} of {n} correct strums exceeds 1 in 200 (Wilson upper {:.3}%)",
        100.0 * wilson(k, n).1
    );
    assert!(
        200 * kr <= nr,
        "reserved condition: Names on {kr} of {nr} exceeds 1 in 200"
    );
}

fn check2(b: &Bench, tag: &str) {
    let t = b
        .errors
        .tally(|r| r.tag.class == Class::Neighbour && r.tag.tonic_differs && r.tag.cond.clean());
    let rate = frac(t.names_true, t.n);
    println!(
        "[{tag}] test 2: neighbour with another tonic, 40 dB dry: named correctly {} of {} = {:.1}% (floor 50%); supports {}, other names {}",
        t.names_true,
        t.n,
        100.0 * rate,
        t.supports,
        t.names_other
    );
    assert!(t.n >= 20, "only {} clean neighbours", t.n);
    assert!(rate > 0.5, "neighbours named in only {:.1}%", 100.0 * rate);
}

/// The control "fails (1)": a witness with a wrong dictionary must not pass the bench. A
/// witness passes when it keeps `Names` within 1 in 200 on correct strums **and** still
/// approves them (`Supports` in at least half): a witness that abstains on everything is quiet
/// but useless, and the bench says so.
fn passes(rep: &BenchReport) -> (bool, usize, usize, f64) {
    let t = rep.tally(|r| r.tag.class == Class::Right);
    let quiet = 200 * t.names() <= t.n;
    let useful = frac(t.supports, t.n) >= 0.5;
    (quiet && useful, t.names(), t.n, frac(t.supports, t.n))
}

fn check3(b: &Bench, tag: &str) {
    let (ok_real, ..) = passes(&b.right_std);
    let (ok_ctl, k, n, sup) = passes(&b.control);
    println!(
        "[{tag}] test 3: dictionary shifted one semitone: Names {k} of {n}, Supports {:.1}% -> {}; real witness {}",
        100.0 * sup,
        if ok_ctl {
            "PASSES (bad)"
        } else {
            "fails (good)"
        },
        if ok_real { "passes" } else { "FAILS" }
    );
    assert!(ok_real, "the real witness must pass its own bar");
    assert!(!ok_ctl, "the shifted dictionary must fail the bench");
}

fn check4(b: &Bench, tag: &str) {
    let t = b.errors.tally(|r| r.tag.class == Class::OutOfSet);
    println!(
        "[{tag}] test 4: out of the set (two wrong frets): Supports {} of {} = {:.1}% ; decide Ok {:.1}% ; named {} ; abstained {}",
        t.supports,
        t.n,
        100.0 * frac(t.supports, t.n),
        100.0 * frac(t.ok, t.n),
        t.names(),
        t.abstained()
    );
    assert!(t.n >= 20, "only {} out-of-set strums", t.n);
    // K1: nothing this bench measures may unlock a chord gate.
    let rep = merged(b).confusion(&open_gates(), |_| true);
    let proposed = rep.proposed_gates(&GateTable::builtin());
    for t in [
        VerdictType::ChordIdentity,
        VerdictType::StringMissing,
        VerdictType::StringExtra,
        VerdictType::WrongFret,
    ] {
        assert_eq!(
            proposed.state(t),
            guitarness_core::gates::GateState::Locked,
            "{t:?} unlocked by synthetic evidence"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Tests

#[test]
fn bench_seeds_are_outside_the_development_range() {
    const { assert!(RESERVED_SEEDS >= DEV_SEEDS.end) };
    let all = positions();
    let pos: Vec<&Pos> = all.iter().collect();
    for sc in right_grid(&pos, &std_conds(), 0..3)
        .iter()
        .chain(&error_grid(&pos, 100..102, &[0], 1))
        .chain(&right_grid(&pos, &reserved_conds(), 5..25))
    {
        assert!(
            sc.tag.seed >= DEV_SEEDS.end,
            "seed {} in the dev range",
            sc.tag.seed
        );
    }
}

#[test]
fn subset_t1_names_on_correct_strums() {
    let b = subset();
    println!("subset built in {:.1} s", b.secs);
    check1(b, 200, "subset");
}

#[test]
fn subset_t2_neighbour_named() {
    check2(subset(), "subset");
}

#[test]
fn subset_t3_shifted_dictionary_fails() {
    check3(subset(), "subset");
}

#[test]
fn subset_t4_out_of_set_reported() {
    check4(subset(), "subset");
}

#[test]
#[ignore = "long bench; run with --release -- --ignored --nocapture"]
fn full_t1_names_on_correct_strums() {
    check1(full(), 2000, "full");
}

#[test]
#[ignore = "long bench; run with --release -- --ignored --nocapture"]
fn full_t2_neighbour_named() {
    check2(full(), "full");
}

#[test]
#[ignore = "long bench; run with --release -- --ignored --nocapture"]
fn full_t3_shifted_dictionary_fails() {
    check3(full(), "full");
}

#[test]
#[ignore = "long bench; run with --release -- --ignored --nocapture"]
fn full_t4_out_of_set_reported() {
    check4(full(), "full");
}

/// Print the per-pair table; write it to `$BENCH_TABLE` when set.
#[test]
#[ignore = "long bench; run with --release -- --ignored --nocapture"]
fn full_table() {
    let b = full();
    let rep = merged(b);
    let table = rep.table_md(&open_gates());
    println!(
        "{} records, {:.2} ms per witness call, bench {:.0} s",
        rep.records.len(),
        rep.mean_opine_ms(),
        b.secs
    );
    if let Ok(path) = std::env::var("BENCH_TABLE") {
        std::fs::write(&path, &table).expect("write table");
        println!("table written to {path}");
    } else {
        println!("{table}");
    }
}

// ---------------------------------------------------------------------------------------
// Detune sweep

/// Hands that ring: the error grid keeps only WrongFret and Extra for the detune table
/// (Missing, Neighbour and AllMuted are not what the sweep asks about).
fn keep_named_classes(g: Vec<Scenario>) -> Vec<Scenario> {
    g.into_iter()
        .filter(|s| matches!(s.tag.class, Class::WrongFret | Class::Extra))
        .collect()
}

/// One row of the detune table.
struct DetuneRow {
    name: &'static str,
    n: usize,
    names: usize,
    supports: usize,
    wf_n: usize,
    wf_named: usize,
    wf_supports: usize,
    x_n: usize,
    x_named: usize,
    x_supports: usize,
    detuned: usize,
}

impl DetuneRow {
    fn md(&self) -> String {
        let pc = |k: usize, n: usize| format!("{:.1}", 100.0 * frac(k, n));
        format!(
            "| {} | {} | {} | {:.3} | {:.3} | {} | {} | {} | {} | {:.3} | {} | {} | {} | {:.3} |\n",
            self.name,
            self.n,
            self.names,
            100.0 * frac(self.names, self.n),
            100.0 * wilson(self.names, self.n).1,
            pc(self.supports, self.n),
            self.wf_n,
            pc(self.wf_named, self.wf_n),
            self.wf_supports,
            100.0 * wilson(self.wf_supports, self.wf_n).1,
            self.x_n,
            pc(self.x_named, self.x_n),
            self.x_supports,
            100.0 * wilson(self.x_supports, self.x_n).1,
        )
    }
}

const DETUNE_HEAD: &str = "| condition | correct n | Names | Names % | Wilson 95% upper % | Supports % | \
     WrongFret n | WrongFret named % | WrongFret Supports | its Wilson upper % | Extra n | \
     Extra named % | Extra Supports | its Wilson upper % |\n\
     |---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n";

/// The conditions of the sweep. The first seven keep the seeds of the V4b table.
const DETUNES: [(&str, Detune); 9] = [
    ("exact 440 Hz (control)", Detune::None),
    ("whole guitar -30 cents", Detune::Offset(-30.0)),
    ("whole guitar -15 cents", Detune::Offset(-15.0)),
    ("whole guitar +15 cents", Detune::Offset(15.0)),
    ("whole guitar +30 cents", Detune::Offset(30.0)),
    ("per string sigma 5 cents", Detune::Sigma(5.0)),
    ("per string sigma 10 cents", Detune::Sigma(10.0)),
    ("whole guitar -30 cents, sigma 5", Detune::Both(-30.0, 5.0)),
    ("whole guitar +30 cents, sigma 5", Detune::Both(30.0, 5.0)),
];

/// Seed indices of one sweep condition: standard conditions, reserved condition, errors.
struct SweepSeeds {
    std: std::ops::Range<u32>,
    res: std::ops::Range<u32>,
    err: std::ops::Range<u32>,
}

/// Correct strums and WrongFret / Extra strums of one detune condition.
fn detune_row(
    pos: &[&Pos],
    name: &'static str,
    d: Detune,
    seeds: &SweepSeeds,
    combos: &[usize],
    dev: bool,
) -> DetuneRow {
    let cfg = ExciteConfig::default();
    let mut right = run_par(
        right_grid_d(pos, &std_conds(), seeds.std.clone(), d, dev),
        cfg,
    );
    right.merge(run_par(
        right_grid_d(pos, &reserved_conds(), seeds.res.clone(), d, dev),
        cfg,
    ));
    let errs = run_par(
        keep_named_classes(error_grid_d(
            pos,
            seeds.err.clone(),
            combos,
            1,
            d,
            true,
            dev,
        )),
        cfg,
    );
    let r = right.tally(|r| r.tag.class == Class::Right);
    let w = errs.tally(|r| r.tag.class == Class::WrongFret);
    let x = errs.tally(|r| r.tag.class == Class::Extra);
    DetuneRow {
        name,
        n: r.n,
        names: r.names(),
        supports: r.supports,
        wf_n: w.n,
        wf_named: w.names(),
        wf_supports: w.supports,
        x_n: x.n,
        x_named: x.names(),
        x_supports: x.supports,
        detuned: right
            .records
            .iter()
            .filter(|rec| {
                rec.tag.class == Class::Right && rec.dsp == Opinion::Abstains(Abstain::Unavailable)
            })
            .count(),
    }
}

/// The V4b sweep plan for condition `i`: seed indices 200 + 20 i onward.
fn sweep_seeds(i: usize) -> SweepSeeds {
    let base = 200 + 20 * i as u32;
    SweepSeeds {
        std: base..base + 3,
        res: base + 10..base + 20,
        err: base + 5..base + 8,
    }
}

/// The V2c sweep plan for condition `i`, a block no build has seen: seed indices 1000 + 100 i
/// onward, four times the V4b counts (8160 correct strums per condition).
fn fresh_seeds(i: usize) -> SweepSeeds {
    let base = 1000 + 100 * i as u32;
    SweepSeeds {
        std: base..base + 12,
        res: base + 20..base + 60,
        err: base + 70..base + 82,
    }
}

/// How the witness copes with a guitar that is not at 440 Hz: false `Names` and `Supports` on
/// correct strums, and the share of WrongFret / Extra strums with a name. Measurement only.
/// Write the table to `$DETUNE_TABLE` when set. Seeds are held out (never used to tune).
/// `cargo test -p guitarness-verify --release --test bench_synth detune_sweep -- --ignored --nocapture`
#[test]
#[ignore = "long bench; run with --release -- --ignored --nocapture"]
fn detune_sweep() {
    let all = positions();
    let pos: Vec<&Pos> = all.iter().collect();
    let mut md = String::from(DETUNE_HEAD);
    let mut unavailable = String::new();
    for (i, (name, d)) in DETUNES.iter().enumerate() {
        let t = std::time::Instant::now();
        let row = detune_row(&pos, name, *d, &fresh_seeds(i), &[0, 3, 4, 6], false);
        md += &row.md();
        unavailable += &format!("{name}: {} of {} unavailable\n", row.detuned, row.n);
        println!("{name}: done in {:.0} s", t.elapsed().as_secs_f64());
    }
    md += "\nSeeds 1,000,000 + 100,000 x [1000, 1882) (fresh V2c block). Strum RMS, noise and reverb as the bench; the \
           detune is applied per string by cubic resampling of the rendered string (time scale \
           changes by the same ratio, at most 1.7%). Inharmonicity: not available through the \
           public API of guitarness-sim (Karplus-Strong loop, no dispersion parameter), not measured.\n";
    println!("{md}\n{unavailable}");
    if let Ok(path) = std::env::var("DETUNE_TABLE") {
        std::fs::write(&path, &md).expect("write table");
    }
}

/// The same sweep on seeds moved into `DEV_SEEDS`: what the tuning fit was chosen on.
#[test]
#[ignore = "development sweep; run with --release -- --ignored --nocapture"]
fn detune_dev() {
    let all = positions();
    let pos: Vec<&Pos> = all.iter().collect();
    let only: Option<usize> = std::env::var("DETUNE_ONLY")
        .ok()
        .and_then(|v| v.parse().ok());
    let mut md = String::from(DETUNE_HEAD);
    for (i, (name, d)) in DETUNES.iter().enumerate() {
        if only.is_some_and(|k| k != i) {
            continue;
        }
        let row = detune_row(&pos, name, *d, &sweep_seeds(i), &[0, 3, 4, 6], true);
        md += &row.md();
        println!("{}", row.md().trim_end());
        println!("   unavailable {} of {}", row.detuned, row.n);
    }
    println!("{md}");
}

/// A small held-out grid of the sweep on every build: one position per chord, its own seeds.
/// Bounds: false `Names` within 1 in 200 everywhere, `Supports` kept on a guitar that is in
/// tune, uniformly off by 15 cents, or off by 5 cents per string.
#[test]
fn detune_subset() {
    let all = positions();
    let mut seen = Vec::new();
    let pos: Vec<&Pos> = all
        .iter()
        .filter(|p| {
            let fresh = !seen.contains(&p.chord);
            seen.push(p.chord);
            fresh
        })
        .collect();
    let picks = [0usize, 2, 5, 8];
    for (k, &i) in picks.iter().enumerate() {
        let (name, d) = DETUNES[i];
        let base = 600 + 10 * k as u32;
        let seeds = SweepSeeds {
            std: base..base + 1,
            res: base + 1..base + 3,
            err: base + 3..base + 4,
        };
        let row = detune_row(&pos, name, d, &seeds, &[0, 6], false);
        print!("{}", row.md());
        assert!(row.n >= 100, "{name}: {} correct strums", row.n);
        assert!(
            200 * row.names <= row.n,
            "{name}: Names on {} of {} correct strums",
            row.names,
            row.n
        );
        let floor = if i == 8 { 0.4 } else { 0.5 };
        assert!(
            frac(row.supports, row.n) >= floor,
            "{name}: Supports {} of {}",
            row.supports,
            row.n
        );
    }
}
