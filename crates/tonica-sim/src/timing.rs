//! Synthetic timing bench for the onset detector (DESIGN §1.11, "Tempo": bias ≤ 2 ms,
//! σ ≤ 4 ms; presence: misses and extras ≤ 0.5%).
//!
//! A take is a strummed rhythm (chord progression, down/up pattern, muted strums, partial up
//! strums, random spread, dynamics and attack softness) played with human timing error over
//! a beat grid, mixed with room reverb, pink noise and hum at a given SNR, an optional
//! metronome leaking from the speakers (> 6 kHz), and a laptop-mic high-pass.
//! The truth is the first string of every strum.

use tonica_dsp::biquad::{Biquad, Kind};
use tonica_dsp::synth::Mulberry32;
use tonica_dsp::{OnsetConfig, OnsetDetector, OnsetEvent};

use crate::fx::{click, game_click, gauss, hum, laptop_mic, mix_at, pink, reverb, rms, uni};
use crate::string::{Pluck, render, render_glide};
use crate::strum::{Dir, OPEN, StrumSpec, render_take_with, voicing};

const PROGRESSION: [&str; 9] = ["G", "C", "D", "Em", "Am", "F", "A", "E", "Dm"];
/// Guitar strum level after normalisation (RMS of the first 50 ms).
const STRUM_RMS: f64 = 0.1;

/// A finger landing on a ringing string without the picking hand. The pitch jumps to the new
/// fret, and the string hitting the fret is a small excitation of its own.
#[derive(Clone, Copy, Debug)]
pub struct Hammer {
    /// Level of the hammered note below the same string picked, dB (RMS of the first 20 ms).
    pub level_db: f64,
    /// Softness of the fingertip: the excitation is low-passed (12 dB/oct) at this
    /// frequency, Hz. Above 0.45 × the sample rate: no low-pass (string slapped on the fret).
    pub lp_hz: f64,
}

/// Fingers *placed* during a chord change on strings that still ring. Rough estimate, not a
/// measurement: a pick stroke that pulls a string 1 to 2 mm aside sets it moving at 1 to
/// 5 m/s; a fingertip laid down at 0.05 to 0.1 m/s is 20 to 40 dB below that, and its pad is
/// soft. The detector must not hear this (asserted in the regressions).
pub const FINGERS_PLACED: Hammer = Hammer {
    level_db: -36.0,
    lp_hz: 1500.0,
};

/// A firm hammer-on (about 1 m/s, the string slapped on the fret): a note in its own right,
/// as loud as a soft pick stroke. The detector counts it; no evidence in the signal tells it
/// from a soft single-string strum (see `hammer_limit` in `tests/diag.rs` for the limit).
pub const HAMMER_FIRM: Hammer = Hammer {
    level_db: -12.0,
    lp_hz: f64::INFINITY,
};

#[derive(Clone, Debug)]
pub struct Cond {
    pub name: String,
    pub sr: f64,
    pub bpm: f64,
    /// Strums per beat (2 = eighths, 4 = sixteenths).
    pub per_beat: u32,
    pub events: usize,
    pub snr_db: f64,
    pub reverb_wet: f64,
    pub mute_frac: f64,
    pub metronome: bool,
    /// Metronome level relative to the strum level, dB.
    pub click_db: f64,
    /// Dynamic range of the strums, dB below full.
    pub dyn_db: f64,
    /// Player timing error σ, seconds.
    pub human_sigma: f64,
    pub seed: u32,
}

impl Cond {
    pub fn base(name: &str, seed: u32) -> Self {
        Self {
            name: name.into(),
            sr: 48000.0,
            bpm: 90.0,
            per_beat: 2,
            events: 24,
            snr_db: 30.0,
            reverb_wet: 0.0,
            mute_frac: 0.0,
            metronome: false,
            click_db: -6.0,
            dyn_db: 12.0,
            human_sigma: 0.012,
            seed,
        }
    }
}

pub struct Take {
    pub sr: f64,
    pub audio: Vec<f32>,
    /// True onsets, input samples.
    pub truth: Vec<f64>,
    /// True first-to-last string time per strum, seconds.
    pub spread: Vec<f64>,
}

/// The guitar alone, before the room: for diagnostics of what a detector could know.
pub struct Stems {
    /// Dry guitar at the take's level (no reverb, noise, click or microphone).
    pub dry: Vec<f32>,
    /// Only the strings struck by each strum, for the first 45 ms after the strum starts.
    pub fresh: Vec<f32>,
    /// What was played.
    pub specs: Vec<StrumSpec>,
}

/// Build the audio for a condition.
pub fn make_take(c: &Cond) -> Take {
    make_take_stems(c).0
}

/// As [`make_take`], with the dry stems.
pub fn make_take_stems(c: &Cond) -> (Take, Stems) {
    let mut rng = Mulberry32::new(c.seed.wrapping_mul(2_654_435_761).wrapping_add(17));
    let sr = c.sr;
    let ioi = 60.0 / c.bpm / c.per_beat as f64;
    let lead = 0.6;
    let mut specs = Vec::with_capacity(c.events);
    let mut beats = Vec::new();
    for k in 0..c.events {
        let grid = lead + k as f64 * ioi;
        if (k as u32).is_multiple_of(c.per_beat) {
            beats.push(grid);
        }
        let dir = if k % 2 == 0 { Dir::Down } else { Dir::Up };
        let muted = uni(&mut rng, 0.0, 1.0) < c.mute_frac;
        let bar = k / (4 * c.per_beat as usize);
        let v = voicing(PROGRESSION[(bar + c.seed as usize) % PROGRESSION.len()]);
        let strings = match dir {
            Dir::Down => [true; 6],
            // up strums usually catch only the treble strings
            Dir::Up if uni(&mut rng, 0.0, 1.0) < 0.6 => [false, false, true, true, true, true],
            Dir::Up => [true; 6],
        };
        let spread = match dir {
            Dir::Down => uni(&mut rng, 0.005, 0.040),
            Dir::Up => uni(&mut rng, 0.005, 0.025),
        };
        let t0 = grid + (gauss(&mut rng) * c.human_sigma).clamp(-0.3 * ioi, 0.3 * ioi);
        specs.push(StrumSpec {
            t0,
            voicing: v,
            dir,
            spread,
            gain: 10f64.powf(-uni(&mut rng, 0.0, c.dyn_db) / 20.0),
            muted,
            strings,
            attack: uni(&mut rng, 0.0, 0.0025),
        });
    }
    let len = ((lead + c.events as f64 * ioi + 1.0) * sr) as usize;
    let mut fresh = vec![0.0f32; len];
    let keep = (0.045 * sr) as usize;
    let r = render_take_with(sr, &specs, len, &mut rng, |k, start, y| {
        let end = (specs[k].t0 * sr) as usize + keep;
        if end > start {
            mix_at(&mut fresh, &y[..y.len().min(end - start)], start, 1.0);
        }
    });
    let mut audio = r.audio;

    // Normalise so the median strum's first 50 ms sit at STRUM_RMS.
    let w = (0.05 * sr) as usize;
    let mut lv: Vec<f64> = r
        .onsets
        .iter()
        .map(|&o| {
            let a = o as usize;
            rms(&audio[a.min(len)..(a + w).min(len)])
        })
        .collect();
    lv.sort_by(f64::total_cmp);
    let level = lv[lv.len() / 2].max(1e-9);
    audio
        .iter_mut()
        .for_each(|v| *v = (*v as f64 * STRUM_RMS / level) as f32);
    fresh
        .iter_mut()
        .for_each(|v| *v = (*v as f64 * STRUM_RMS / level) as f32);
    let dry = audio.clone();

    if c.metronome {
        let ck = click(sr);
        let g = STRUM_RMS * 10f64.powf(c.click_db / 20.0) / rms(&ck);
        for &b in &beats {
            mix_at(&mut audio, &ck, (b * sr) as usize, g);
        }
    }
    reverb(&mut audio, sr, 0.5, c.reverb_wet);
    let noise_rms = STRUM_RMS * 10f64.powf(-c.snr_db / 20.0);
    let n = pink(len, &mut rng);
    let h = hum(len, sr, 60.0);
    for ((a, nv), hv) in audio.iter_mut().zip(&n).zip(&h) {
        *a += (noise_rms * (*nv as f64 + 0.5 * *hv as f64)) as f32;
    }
    laptop_mic(&mut audio, sr);

    let spread = r
        .string_times
        .iter()
        .map(|t| t.last().copied().unwrap_or(0.0) - t.first().copied().unwrap_or(0.0))
        .collect();
    (
        Take {
            sr,
            audio,
            truth: r.onsets,
            spread,
        },
        Stems { dry, fresh, specs },
    )
}

#[derive(Clone, Debug, Default)]
pub struct Detected {
    /// Strum times, input samples.
    pub strums: Vec<f64>,
    pub spreads: Vec<f64>,
    /// (retro-dated time, emitted at), input samples.
    pub heard: Vec<(f64, f64)>,
    /// Every event, in the order the detector sent them.
    pub events: Vec<OnsetEvent>,
    pub rejected: u64,
}

pub fn detect(take: &Take, cfg: &OnsetConfig) -> Detected {
    let mut d = OnsetDetector::new(take.sr, cfg.clone());
    let mut out = Detected::default();
    let on = |e: OnsetEvent, out: &mut Detected| {
        out.events.push(e);
        match e {
            OnsetEvent::Strum(s) => {
                out.strums.push(s.t);
                out.spreads.push(s.spread);
            }
            OnsetEvent::Heard { t, at } => out.heard.push((t, at)),
        }
    };
    for b in take.audio.chunks(256) {
        d.process(b, |e| on(e, &mut out));
    }
    d.flush(|e| on(e, &mut out));
    out.rejected = d.rejected();
    out
}

/// The early notice is in time for the flash if it leaves within this of the attack, ms.
pub const HEARD_BUDGET_MS: f64 = 11.0;
/// A notice names an attack of its own strum. Not one before it: the strum is dated at its
/// first string that rises by `rise_rel` of the largest rise, so the notice may name an
/// earlier, weaker string, but an attack `group_gap_s` (45 ms) before would be another strum…
pub const HEARD_BEFORE_MS: f64 = 45.0;
/// …and not one after the last string that the strum counted, to within one rise window.
pub const HEARD_AFTER_MS: f64 = 2.0;

/// What a stream of events breaks of the contract of [`OnsetEvent`]: every `Strum` directly
/// follows a `Heard` of its own (so no two strums share one), that notice names an attack of
/// the strum (less than `HEARD_BEFORE_MS` before its onset, at most `HEARD_AFTER_MS` after
/// its last string) and did not leave before the onset it names; notices leave in time
/// order and strums are dated in time order. Empty when the contract holds.
pub fn stream_faults(events: &[OnsetEvent], sr: f64) -> Vec<String> {
    let ms = |v: f64| v / sr * 1000.0;
    let mut out = Vec::new();
    let (mut last_at, mut last_strum) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (i, e) in events.iter().enumerate() {
        match *e {
            OnsetEvent::Heard { t, at } => {
                if at < t {
                    out.push(format!(
                        "#{i}: notice left {:.2} ms before its onset",
                        ms(t - at)
                    ));
                }
                if at < last_at {
                    out.push(format!("#{i}: notice left before the one before it"));
                }
                last_at = at;
            }
            OnsetEvent::Strum(s) => {
                if s.t <= last_strum {
                    out.push(format!("#{i}: strum dated before the one before it"));
                }
                last_strum = s.t;
                match i.checked_sub(1).map(|k| events[k]) {
                    Some(OnsetEvent::Heard { t, .. }) => {
                        let lead = ms(t - s.t);
                        if lead <= -HEARD_BEFORE_MS || lead > ms(s.spread) + HEARD_AFTER_MS {
                            out.push(format!(
                                "#{i}: notice {lead:+.1} ms from its strum (spread {:.1} ms)",
                                ms(s.spread)
                            ));
                        }
                    }
                    _ => out.push(format!("#{i}: strum without a notice of its own")),
                }
            }
        }
    }
    out
}

/// Notices that no strum followed (a flash without a strum).
pub fn unpaired_heard(events: &[OnsetEvent]) -> usize {
    events
        .iter()
        .enumerate()
        .filter(|(i, e)| {
            matches!(e, OnsetEvent::Heard { .. })
                && !matches!(events.get(i + 1), Some(OnsetEvent::Strum(_)))
        })
        .count()
}

#[derive(Clone, Debug, Default)]
pub struct Score {
    pub n: usize,
    pub hits: usize,
    pub extras: usize,
    /// Detected − true, ms, for hits.
    pub err_ms: Vec<f64>,
    /// Heard emission − true onset, ms.
    pub heard_ms: Vec<f64>,
    /// (true spread, detected spread), ms.
    pub spread_ms: Vec<(f64, f64)>,
    /// Notices that no strum followed.
    pub unpaired: usize,
    /// Breaches of the event contract ([`stream_faults`]).
    pub faults: Vec<String>,
}

impl Score {
    pub fn add(&mut self, o: &Score) {
        self.n += o.n;
        self.hits += o.hits;
        self.extras += o.extras;
        self.err_ms.extend_from_slice(&o.err_ms);
        self.heard_ms.extend_from_slice(&o.heard_ms);
        self.spread_ms.extend_from_slice(&o.spread_ms);
        self.unpaired += o.unpaired;
        self.faults.extend_from_slice(&o.faults);
    }
}

/// One-to-one matching in time order within ±`tol_ms`.
pub fn score(take: &Take, det: &Detected, tol_ms: f64) -> Score {
    let ms = |s: f64| s / take.sr * 1000.0;
    let mut sc = Score {
        n: take.truth.len(),
        ..Default::default()
    };
    let mut used = vec![false; det.strums.len()];
    let mut used_h = vec![false; det.heard.len()];
    for (i, &t) in take.truth.iter().enumerate() {
        let best = det
            .strums
            .iter()
            .enumerate()
            .filter(|(k, d)| !used[*k] && ms((*d - t).abs()) <= tol_ms)
            .min_by(|a, b| (a.1 - t).abs().total_cmp(&(b.1 - t).abs()));
        if let Some((k, &d)) = best {
            used[k] = true;
            sc.hits += 1;
            sc.err_ms.push(ms(d - t));
            sc.spread_ms
                .push((take.spread[i] * 1000.0, ms(det.spreads[k])));
        }
        let hb = det
            .heard
            .iter()
            .enumerate()
            .filter(|(k, h)| !used_h[*k] && ms((h.0 - t).abs()) <= tol_ms)
            .min_by(|a, b| (a.1.0 - t).abs().total_cmp(&(b.1.0 - t).abs()));
        if let Some((k, h)) = hb {
            used_h[k] = true;
            sc.heard_ms.push(ms(h.1 - t));
        }
    }
    sc.extras = used.iter().filter(|u| !**u).count();
    sc.unpaired = unpaired_heard(&det.events);
    sc.faults = stream_faults(&det.events, take.sr);
    sc
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Summary {
    pub n: usize,
    pub recall: f64,
    pub extra_rate: f64,
    pub bias_ms: f64,
    pub sigma_ms: f64,
    pub p95_abs_ms: f64,
    pub max_abs_ms: f64,
    /// Notice emitted − true attack, ms, over the strums that got a notice.
    pub heard_p95_ms: f64,
    /// Share of all true strums with a notice within `HEARD_BUDGET_MS` of the attack.
    pub heard_in_time: f64,
    /// Earliest notice relative to its attack, ms (negative: a flash before the strum).
    pub heard_min_ms: f64,
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * q).round() as usize]
}

pub fn summarize(s: &Score) -> Summary {
    let m = s.err_ms.len().max(1) as f64;
    let bias = s.err_ms.iter().sum::<f64>() / m;
    let var = s.err_ms.iter().map(|e| (e - bias).powi(2)).sum::<f64>() / m;
    let mut abs: Vec<f64> = s.err_ms.iter().map(|e| e.abs()).collect();
    let mut h = s.heard_ms.clone();
    Summary {
        n: s.n,
        recall: s.hits as f64 / s.n.max(1) as f64,
        extra_rate: s.extras as f64 / s.n.max(1) as f64,
        bias_ms: bias,
        sigma_ms: var.sqrt(),
        p95_abs_ms: pct(&mut abs, 0.95),
        max_abs_ms: abs.last().copied().unwrap_or(f64::NAN),
        heard_p95_ms: pct(&mut h, 0.95),
        heard_in_time: h.iter().filter(|v| **v <= HEARD_BUDGET_MS).count() as f64
            / s.n.max(1) as f64,
        heard_min_ms: h.first().copied().unwrap_or(f64::NAN),
    }
}

/// The standard grid (DESIGN §1.11a, timing part). `quick` keeps one seed per cell.
pub fn standard_grid(quick: bool) -> Vec<Cond> {
    let mut out = Vec::new();
    let tempos: [(f64, u32); 4] = [(60.0, 2), (100.0, 2), (120.0, 4), (180.0, 2)];
    let seeds: &[u32] = if quick { &[1] } else { &[1, 2, 3] };
    let mut id = 0u32;
    for &snr in &[40.0, 30.0, 20.0] {
        for &wet in &[0.0, 0.25] {
            for &mute in &[0.0, 0.3] {
                for &(bpm, pb) in &tempos {
                    for &metro in &[false, true] {
                        for &s in seeds {
                            id += 1;
                            let mut c = Cond::base(
                                &format!(
                                    "snr{snr:.0}_rev{wet:.2}_mute{mute:.1}_{bpm:.0}x{pb}{}",
                                    if metro { "_click" } else { "" }
                                ),
                                id * 7 + s,
                            );
                            c.snr_db = snr;
                            c.reverb_wet = wet;
                            c.mute_frac = mute;
                            c.bpm = bpm;
                            c.per_beat = pb;
                            c.metronome = metro;
                            out.push(c);
                        }
                    }
                }
            }
        }
    }
    // one 44.1 kHz cell per SNR
    for &snr in &[40.0, 30.0, 20.0] {
        id += 1;
        let mut c = Cond::base(&format!("sr44k_snr{snr:.0}"), id * 7);
        c.sr = 44100.0;
        c.snr_db = snr;
        out.push(c);
    }
    out
}

/// A no-strum regression: audio that must produce exactly `expect` strums.
pub struct Regression {
    pub name: &'static str,
    pub take: Take,
    pub expect: usize,
}

pub fn regressions() -> Vec<Regression> {
    let sr = 48000.0;
    let mut out = Vec::new();
    let mut rng = Mulberry32::new(99);

    // no_strum_at_all: room noise and hum only (level as for a 30 dB SNR take).
    let len = (8.0 * sr) as usize;
    let noise_rms = STRUM_RMS * 10f64.powf(-30.0 / 20.0);
    let mut a: Vec<f32> = pink(len, &mut rng)
        .iter()
        .map(|v| (*v as f64 * noise_rms) as f32)
        .collect();
    let h = hum(len, sr, 60.0);
    a.iter_mut()
        .zip(&h)
        .for_each(|(v, x)| *v += (*x as f64 * noise_rms * 0.5) as f32);
    laptop_mic(&mut a, sr);
    out.push(Regression {
        name: "no_strum_at_all",
        take: bare(sr, a),
        expect: 0,
    });

    // metronome_leak_no_strum: loud clicks (0 dB re strum level) + reverb + noise, no guitar.
    let mut a = vec![0.0f32; len];
    let ck = click(sr);
    let g = STRUM_RMS / rms(&ck);
    let mut t = 0.5;
    while t < 7.5 {
        mix_at(&mut a, &ck, (t * sr) as usize, g);
        t += 0.5;
    }
    reverb(&mut a, sr, 0.5, 0.25);
    let n = pink(len, &mut rng);
    a.iter_mut()
        .zip(&n)
        .for_each(|(v, x)| *v += (*x as f64 * noise_rms) as f32);
    laptop_mic(&mut a, sr);
    out.push(Regression {
        name: "metronome_leak_no_strum",
        take: bare(sr, a),
        expect: 0,
    });

    // metronome_digital_silence: the game's own click and accent, straight into the detector
    // at full level with exact zeros in between (muted microphone, loopback, noise gate).
    for (name, fs) in [
        ("metronome_digital_silence_48k", 48000.0),
        ("metronome_digital_silence_44k", 44100.0),
    ] {
        let mut a = vec![0.0f32; (8.0 * fs) as usize];
        let (ck, acc) = (game_click(fs, false), game_click(fs, true));
        let mut k = 0;
        while 0.1 + 0.333 * k as f64 + 0.02 < 8.0 {
            mix_at(
                &mut a,
                if k % 4 == 0 { &acc } else { &ck },
                ((0.1 + 0.333 * k as f64) * fs) as usize,
                1.0,
            );
            k += 1;
        }
        out.push(Regression {
            name,
            take: bare(fs, a),
            expect: 0,
        });
    }

    // sustain_only: one G strum, then 5 s of ringing with noise.
    let mut c = Cond::base("sustain", 5);
    c.events = 1;
    c.human_sigma = 0.0;
    let mut tk = make_take(&c);
    tk.audio.resize((6.0 * sr) as usize, 0.0);
    let n = pink(tk.audio.len(), &mut rng);
    let tail = (1.6 * sr) as usize;
    tk.audio[tail..]
        .iter_mut()
        .zip(&n[tail..])
        .for_each(|(v, x)| *v += (*x as f64 * noise_rms) as f32);
    out.push(Regression {
        name: "sustain_only",
        take: tk,
        expect: 1,
    });

    // left_hand_only: one strum of C, then the fretting hand changes to Am, G and D shapes
    // without strumming (pitch changes, no new excitation). Expect only the first strum.
    let len = (5.0 * sr) as usize;
    let mut a = vec![0.0f32; len];
    let c_shape = voicing("C").0;
    let shapes = [voicing("Am").0, voicing("G").0, voicing("D").0];
    for s in 0..6 {
        let f0 = c_shape[s];
        if f0 < 0 {
            continue;
        }
        let mut p = Pluck::new(tonica_dsp::mtof((OPEN[s] + f0 as i32) as f64, 440.0));
        p.t60 = 4.0;
        p.gain = 0.6;
        let at = (0.5 * sr + s as f64 * 0.004 * sr) as usize;
        // one re-fret per string at 1.5 s (Am), using the new shape when it is fretted there
        let nf = shapes[0][s].max(0);
        let refret = [(
            (1.0 * sr) as usize,
            tonica_dsp::mtof((OPEN[s] + nf as i32) as f64, 440.0),
        )];
        let y = render(sr, &p, len - at, None, &refret, &mut rng);
        mix_at(&mut a, &y, at, 1.0);
    }
    let lvl = rms(&a[(0.5 * sr) as usize..(0.55 * sr) as usize]).max(1e-9);
    a.iter_mut()
        .for_each(|v| *v = (*v as f64 * STRUM_RMS / lvl) as f32);
    let n = pink(len, &mut rng);
    a.iter_mut()
        .zip(&n)
        .for_each(|(v, x)| *v += (*x as f64 * noise_rms) as f32);
    laptop_mic(&mut a, sr);
    let mut tk = bare(sr, a);
    tk.truth = vec![0.5 * sr];
    tk.spread = vec![0.02];
    out.push(Regression {
        name: "left_hand_only",
        take: tk,
        expect: 1,
    });

    // Harder left-hand variants. All start with one real strum at 0.5 s (4 ms per string,
    // strings as bright and as varied as in the takes) and then only the fretting hand moves.
    let len = (5.5 * sr) as usize;
    let strum_at = 0.5;
    let fret_freq =
        |s: usize, fret: i8| tonica_dsp::mtof((OPEN[s] + fret.max(0) as i32) as f64, 440.0);
    let take_pluck = |s: usize, fret: i8, rng: &mut Mulberry32| {
        let mut p = Pluck::new(fret_freq(s, fret));
        p.gain = uni(rng, 0.55, 1.0);
        p.damp = uni(rng, 0.25, 0.6);
        p.soft = uni(rng, 0.0, 0.5);
        p.pick_pos = uni(rng, 0.08, 0.25);
        p.t60 = uni(rng, 2.0, 5.0) * (110.0 / p.freq).sqrt();
        p
    };
    let finish = |mut a: Vec<f32>, rng: &mut Mulberry32| {
        let lvl = rms(&a[(strum_at * sr) as usize..((strum_at + 0.05) * sr) as usize]).max(1e-9);
        a.iter_mut()
            .for_each(|v| *v = (*v as f64 * STRUM_RMS / lvl) as f32);
        let n = pink(a.len(), rng);
        a.iter_mut()
            .zip(&n)
            .for_each(|(v, x)| *v += (*x as f64 * noise_rms) as f32);
        laptop_mic(&mut a, sr);
        let mut tk = bare(sr, a);
        tk.truth = vec![strum_at * sr];
        tk.spread = vec![0.02];
        tk
    };
    let start_of = |s: usize| ((strum_at + s as f64 * 0.004) * sr) as usize;

    // left_hand_multi: C, then Am, G and D shapes one second apart; the fingers land one
    // after another (up to 60 ms apart), each string jumping to its new fret.
    let mut a = vec![0.0f32; len];
    for s in 0..6 {
        if c_shape[s] < 0 {
            continue;
        }
        let p = take_pluck(s, c_shape[s], &mut rng);
        let at = start_of(s);
        let refrets: Vec<(usize, f64)> = shapes
            .iter()
            .enumerate()
            .map(|(k, sh)| {
                let t = 1.0 + k as f64 + uni(&mut rng, 0.0, 0.060);
                ((t * sr) as usize, fret_freq(s, sh[s]))
            })
            .collect();
        let y = render(sr, &p, len - at, None, &refrets, &mut rng);
        mix_at(&mut a, &y, at, 1.0);
    }
    out.push(Regression {
        name: "left_hand_multi",
        take: finish(a, &mut rng),
        expect: 1,
    });

    // left_hand_early: as above, but the hand starts moving 150 ms after the strum, while
    // the strings are still bright, and changes shape every 300 ms (C, Am, G, D, C, G).
    let early = [
        voicing("Am").0,
        voicing("G").0,
        voicing("D").0,
        voicing("C").0,
        voicing("G").0,
    ];
    let mut a = vec![0.0f32; len];
    for s in 0..6 {
        if c_shape[s] < 0 {
            continue;
        }
        let p = take_pluck(s, c_shape[s], &mut rng);
        let at = start_of(s);
        let refrets: Vec<(usize, f64)> = early
            .iter()
            .enumerate()
            .map(|(k, sh)| {
                let t = 0.150 + 0.300 * k as f64 + uni(&mut rng, 0.0, 0.040);
                ((t * sr) as usize, fret_freq(s, sh[s]))
            })
            .collect();
        let y = render(sr, &p, len - at, None, &refrets, &mut rng);
        mix_at(&mut a, &y, at, 1.0);
    }
    out.push(Regression {
        name: "left_hand_early",
        take: finish(a, &mut rng),
        expect: 1,
    });

    // slide: an E shape slid up two frets in 150 ms, up three more in 80 ms, then back down
    // five in 300 ms. Every string glides; nothing is struck.
    let e_shape = voicing("E").0;
    let glide = |t: f64| {
        let seg = |t0: f64, dur: f64, semis: f64| {
            let u = ((t - t0) / dur).clamp(0.0, 1.0);
            semis * (0.5 - 0.5 * (std::f64::consts::PI * u).cos())
        };
        seg(1.0, 0.150, 2.0) + seg(2.0, 0.080, 3.0) + seg(3.0, 0.300, -5.0)
    };
    let mut a = vec![0.0f32; len];
    for (s, &fret) in e_shape.iter().enumerate() {
        let p = take_pluck(s, fret, &mut rng);
        let at = start_of(s);
        let f0 = p.freq;
        let y = render_glide(
            sr,
            &p,
            len - at,
            |i| f0 * 2f64.powf(glide(i as f64 / sr) / 12.0),
            &mut rng,
        );
        mix_at(&mut a, &y, at, 1.0);
    }
    out.push(Regression {
        name: "slide",
        take: finish(a, &mut rng),
        expect: 1,
    });

    // vibrato_bend: an Am shape; the whole hand shakes (±40 cents at 5.5 Hz, from 1.0 s),
    // then the treble strings are bent a whole tone in 120 ms and released.
    let am_shape = voicing("Am").0;
    let mut a = vec![0.0f32; len];
    for (s, &fret) in am_shape.iter().enumerate() {
        if fret < 0 {
            continue;
        }
        let p = take_pluck(s, fret, &mut rng);
        let at = start_of(s);
        let f0 = p.freq;
        let treble = s >= 3;
        let cents = move |t: f64| {
            let depth = 40.0
                * ((t - 1.0) / 0.2).clamp(0.0, 1.0)
                * (1.0 - ((t - 2.6) / 0.2).clamp(0.0, 1.0));
            let vib = depth * (2.0 * std::f64::consts::PI * 5.5 * t).sin();
            let ramp = |t0: f64, dur: f64| {
                let u = ((t - t0) / dur).clamp(0.0, 1.0);
                0.5 - 0.5 * (std::f64::consts::PI * u).cos()
            };
            let bend = if treble {
                200.0 * (ramp(3.2, 0.120) - ramp(3.9, 0.120))
            } else {
                0.0
            };
            vib + bend
        };
        let y = render_glide(
            sr,
            &p,
            len - at,
            |i| f0 * 2f64.powf(cents(i as f64 / sr) / 1200.0),
            &mut rng,
        );
        mix_at(&mut a, &y, at, 1.0);
    }
    out.push(Regression {
        name: "vibrato_bend",
        take: finish(a, &mut rng),
        expect: 1,
    });

    // fingers_placed: the fingers of a chord change land on ringing strings (pitch jump plus
    // a weak, dull excitation each), in a normal and in a quiet room.
    out.push(Regression {
        name: "fingers_placed",
        take: hammer_take(FINGERS_PLACED, 30.0),
        expect: 1,
    });
    out.push(Regression {
        name: "fingers_placed_quiet_room",
        take: hammer_take(FINGERS_PLACED, 40.0),
        expect: 1,
    });

    // hand_damp: a G shape rings, then the strumming hand rests on the strings (they die in
    // 5 ms) without striking them.
    let g_shape = voicing("G").0;
    let mut a = vec![0.0f32; len];
    for (s, &fret) in g_shape.iter().enumerate() {
        let p = take_pluck(s, fret, &mut rng);
        let at = start_of(s);
        let stop = (1.5 * sr) as usize + s * 48 - at;
        let y = render(sr, &p, len - at, Some(stop), &[], &mut rng);
        mix_at(&mut a, &y, at, 1.0);
    }
    out.push(Regression {
        name: "hand_damp",
        take: finish(a, &mut rng),
        expect: 1,
    });
    out
}

/// One strum of a C shape at 0.5 s; then seven fingers land on ringing strings: four alone
/// (0.6 s apart), then three within 50 ms (a chord change played hard). One strum expected.
pub fn hammer_take(h: Hammer, snr_db: f64) -> Take {
    let sr = 48000.0;
    let mut rng = Mulberry32::new(1234);
    let len = (5.5 * sr) as usize;
    let strum_at = 0.5;
    let c_shape = voicing("C").0;
    let fret_freq =
        |s: usize, fret: i8| tonica_dsp::mtof((OPEN[s] + fret.max(0) as i32) as f64, 440.0);
    let head = (0.020 * sr) as usize;
    let mut a = vec![0.0f32; len];
    // (string, time, frets up)
    let hammers: [(usize, f64, i8); 7] = [
        (3, 1.0, 2),
        (4, 1.6, 2),
        (2, 2.2, 1),
        (5, 2.8, 3),
        (3, 3.6, 1),
        (4, 3.625, 1),
        (5, 3.65, 2),
    ];
    for (s, &fret0) in c_shape.iter().enumerate() {
        if fret0 < 0 {
            continue;
        }
        let mut p = Pluck::new(fret_freq(s, fret0));
        p.gain = uni(&mut rng, 0.55, 1.0);
        p.damp = uni(&mut rng, 0.25, 0.6);
        p.soft = uni(&mut rng, 0.0, 0.5);
        p.pick_pos = uni(&mut rng, 0.08, 0.25);
        p.t60 = uni(&mut rng, 2.0, 5.0) * (110.0 / p.freq).sqrt();
        let at = ((strum_at + s as f64 * 0.004) * sr) as usize;
        let mut fret = fret0;
        let mut refrets = Vec::new();
        for &(hs, t, up) in &hammers {
            if hs != s {
                continue;
            }
            fret += up;
            let i = (t * sr) as usize;
            refrets.push((i - at, fret_freq(s, fret)));
            // the same string picked at the new fret: the level reference
            let mut q = p;
            q.freq = fret_freq(s, fret);
            let picked = rms(&render(sr, &q, head, None, &[], &mut rng));
            // the hammer: struck at the end of the string (no pick-position comb to speak
            // of), as bright as a pick before the fingertip low-pass
            q.soft = 0.0;
            q.pick_pos = 0.5;
            let mut y = render(sr, &q, len - i, None, &[], &mut rng);
            if h.lp_hz < 0.45 * sr {
                let mut lp =
                    Biquad::new(Kind::LowPass, h.lp_hz, std::f64::consts::FRAC_1_SQRT_2, sr);
                y.iter_mut().for_each(|v| *v = lp.tick(*v));
            }
            let g = picked * 10f64.powf(h.level_db / 20.0) / rms(&y[..head]).max(1e-12);
            mix_at(&mut a, &y, i, g);
        }
        let y = render(sr, &p, len - at, None, &refrets, &mut rng);
        mix_at(&mut a, &y, at, 1.0);
    }
    let lvl = rms(&a[(strum_at * sr) as usize..((strum_at + 0.05) * sr) as usize]).max(1e-9);
    a.iter_mut()
        .for_each(|v| *v = (*v as f64 * STRUM_RMS / lvl) as f32);
    let noise_rms = STRUM_RMS * 10f64.powf(-snr_db / 20.0);
    let n = pink(len, &mut rng);
    a.iter_mut()
        .zip(&n)
        .for_each(|(v, x)| *v += (*x as f64 * noise_rms) as f32);
    laptop_mic(&mut a, sr);
    let mut tk = bare(sr, a);
    tk.truth = vec![strum_at * sr];
    tk.spread = vec![0.02];
    tk
}

fn bare(sr: f64, audio: Vec<f32>) -> Take {
    Take {
        sr,
        audio,
        truth: Vec::new(),
        spread: Vec::new(),
    }
}

/// Run the grid; returns per-condition (name, summary) and the aggregate score.
pub fn run_grid(conds: &[Cond], cfg: &OnsetConfig) -> (Vec<(String, Summary)>, Score) {
    let mut all = Score::default();
    let mut rows = Vec::with_capacity(conds.len());
    for c in conds {
        let tk = make_take(c);
        let det = detect(&tk, cfg);
        let mut sc = score(&tk, &det, 60.0);
        for f in sc.faults.iter_mut() {
            *f = format!("{} {f}", c.name);
        }
        rows.push((c.name.clone(), summarize(&sc)));
        all.add(&sc);
    }
    (rows, all)
}
