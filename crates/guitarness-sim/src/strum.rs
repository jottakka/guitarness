//! Strums on a six-string guitar: voicings, string order, spread, muted "chuck" strums,
//! partial strums, and rendering of a whole take with re-strike damping.

use guitarness_dsp::mtof;
use guitarness_dsp::synth::Mulberry32;

use crate::fx::{mix_at, uni};
use crate::string::{Pluck, render};

/// Open strings, index 0 = 6th string (low E).
pub const OPEN: [i32; 6] = [40, 45, 50, 55, 59, 64];

/// Fret per string (index 0 = low E); -1 = not played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Voicing(pub [i8; 6]);

pub fn voicing(name: &str) -> Voicing {
    Voicing(match name {
        "C" => [-1, 3, 2, 0, 1, 0],
        "D" => [-1, -1, 0, 2, 3, 2],
        "E" => [0, 2, 2, 1, 0, 0],
        "G" => [3, 2, 0, 0, 0, 3],
        "A" => [-1, 0, 2, 2, 2, 0],
        "Am" => [-1, 0, 2, 2, 1, 0],
        "Em" => [0, 2, 2, 0, 0, 0],
        "Dm" => [-1, -1, 0, 2, 3, 1],
        "F" => [-1, -1, 3, 2, 1, 1],
        _ => panic!("unknown chord {name}"),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug)]
pub struct StrumSpec {
    /// Time of the first string, seconds.
    pub t0: f64,
    pub voicing: Voicing,
    pub dir: Dir,
    /// First to last string, seconds.
    pub spread: f64,
    /// Overall level (linear).
    pub gain: f64,
    /// Fretting hand mutes the strings: short percussive "chuck".
    pub muted: bool,
    /// Strings the hand actually crosses (partial strums), index 0 = low E.
    pub strings: [bool; 6],
    /// Attack ramp per string, seconds (0 = pick, ~2 ms = soft thumb/finger).
    pub attack: f64,
}

impl StrumSpec {
    /// Strings struck, in time order.
    pub fn order(&self) -> Vec<usize> {
        let mut s: Vec<usize> = (0..6)
            .filter(|&i| self.strings[i] && (self.muted || self.voicing.0[i] >= 0))
            .collect();
        if self.dir == Dir::Up {
            s.reverse();
        }
        s
    }
}

/// A rendered take: audio, the true onset of every strum (first string, in samples), and,
/// per strum, the time of each string struck.
pub struct Rendered {
    pub audio: Vec<f32>,
    pub onsets: Vec<f64>,
    pub string_times: Vec<Vec<f64>>,
}

/// Render strums into `len` samples. A string still ringing is damped when it is struck
/// again; a muted strum damps every ringing string (the fretting hand releases).
pub fn render_take(sr: f64, specs: &[StrumSpec], len: usize, rng: &mut Mulberry32) -> Rendered {
    render_take_with(sr, specs, len, rng, |_, _, _| {})
}

/// As [`render_take`]; `on_part(strum, start, samples)` also receives every component as it is
/// mixed in (each string of each strum, and the burst of a muted strum), for diagnostics that
/// need the new sound and the ringing sound apart.
pub fn render_take_with(
    sr: f64,
    specs: &[StrumSpec],
    len: usize,
    rng: &mut Mulberry32,
    mut on_part: impl FnMut(usize, usize, &[f32]),
) -> Rendered {
    // (start sample, string, pluck, muted, strum)
    let mut notes: Vec<(usize, usize, Pluck, bool, usize)> = Vec::new();
    let mut onsets = Vec::with_capacity(specs.len());
    let mut string_times = Vec::with_capacity(specs.len());
    let mut mute_times: Vec<usize> = Vec::new();
    for (k_strum, sp) in specs.iter().enumerate() {
        let order = sp.order();
        let k = order.len();
        let mut times = Vec::with_capacity(k);
        for (j, &s) in order.iter().enumerate() {
            let u = if k > 1 {
                let base = j as f64 / (k - 1) as f64;
                let jit = if j == 0 || j + 1 == k {
                    0.0
                } else {
                    uni(rng, -0.25, 0.25) / (k - 1) as f64
                };
                base + jit
            } else {
                0.0
            };
            let t = sp.t0 + sp.spread * u;
            times.push(t);
            let fret = sp.voicing.0[s].max(0) as f64;
            let mut p = Pluck::new(mtof(OPEN[s] as f64 + fret, 440.0));
            p.gain =
                sp.gain * uni(rng, 0.55, 1.0) * if sp.dir == Dir::Up && s < 3 { 0.6 } else { 1.0 };
            p.damp = uni(rng, 0.25, 0.6);
            p.soft = uni(rng, 0.0, 0.5);
            p.pick_pos = uni(rng, 0.08, 0.25);
            p.attack = sp.attack * uni(rng, 0.7, 1.3);
            p.t60 = if sp.muted {
                uni(rng, 0.03, 0.07)
            } else {
                uni(rng, 2.0, 5.0) * (110.0 / p.freq).sqrt()
            };
            notes.push(((t * sr).round() as usize, s, p, sp.muted, k_strum));
        }
        if sp.muted {
            mute_times.push((sp.t0 * sr).round() as usize);
        }
        onsets.push(sp.t0 * sr);
        string_times.push(times);
    }

    let mut audio = vec![0.0f32; len];
    for (i, &(start, s, p, muted, k_strum)) in notes.iter().enumerate() {
        if start >= len {
            continue;
        }
        // next strike on the same string, or the next hand mute after this note starts
        let next_same = notes[i + 1..].iter().find(|n| n.1 == s).map(|n| n.0);
        let next_mute = mute_times.iter().copied().find(|&m| m > start + 1);
        let stop = match (next_same, next_mute) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let dur = stop.map_or(len - start, |e| {
            (e - start + (0.006 * sr) as usize).min(len - start)
        });
        let y = render(sr, &p, dur, stop.map(|e| e - start), &[], rng);
        mix_at(&mut audio, &y, start, 1.0);
        on_part(k_strum, start, &y);
        if muted {
            // pick/fingers hitting damped strings: short broadband burst
            let n = (0.006 * sr) as usize;
            let burst: Vec<f32> = (0..n)
                .map(|k| {
                    ((rng.next_f64() * 2.0 - 1.0) * (-(k as f64) / (0.0015 * sr)).exp()) as f32
                })
                .collect();
            let burst: Vec<f32> = burst
                .iter()
                .map(|v| (*v as f64 * p.gain * 0.5) as f32)
                .collect();
            mix_at(&mut audio, &burst, start, 1.0);
            on_part(k_strum, start, &burst);
        }
    }
    Rendered {
        audio,
        onsets,
        string_times,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_follows_direction_and_voicing() {
        let sp = StrumSpec {
            t0: 0.0,
            voicing: voicing("D"),
            dir: Dir::Down,
            spread: 0.02,
            gain: 1.0,
            muted: false,
            strings: [true; 6],
            attack: 0.0,
        };
        assert_eq!(sp.order(), vec![2, 3, 4, 5]);
        let up = StrumSpec { dir: Dir::Up, ..sp };
        assert_eq!(up.order(), vec![5, 4, 3, 2]);
        let chuck = StrumSpec { muted: true, ..sp };
        assert_eq!(chuck.order().len(), 6);
    }
}
