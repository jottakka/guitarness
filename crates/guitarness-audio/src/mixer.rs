//! Output mixer: sample-exact scheduling of clicks, probes and one-shots.
//!
//! Every sound is placed by absolute output frame. The output of a block depends only
//! on the schedule and on the frame indices, never on how the stream is split in blocks.
//! `render` does not allocate, lock, print or make system calls.

use crate::sounds::{SoundBank, SoundId};
use crate::types::OutFrame;

/// Maximum simultaneous one-shot voices (pending ones count too).
pub const VOICES: usize = 32;
/// Maximum simultaneous metronomes.
const TRACKS: usize = 4;
/// Fade length, in seconds, used for late starts, stops and `Silence`.
const FADE_SECONDS: f64 = 0.001;
const NEVER: u64 = u64::MAX;
/// Largest frame index a command may use (2^62). Anything later is rejected, so no sum
/// of a start, a length and a fade can overflow.
const MAX_FRAME: u64 = 1 << 62;

/// Command from the control thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MixCmd {
    /// Play `sound` starting exactly at frame `at`.
    Play {
        tag: u32,
        sound: SoundId,
        at: OutFrame,
        gain: f32,
    },
    /// Metronome: beat `k` starts at `start + round(k * period)` (`period` in frames,
    /// fractional). Every `accent_every`-th beat (from beat 0) is accented; 0 means never.
    /// `beats` = 0 means no end.
    Metronome {
        tag: u32,
        start: OutFrame,
        period: f64,
        accent_every: u8,
        beats: u32,
        gain: f32,
    },
    /// Stop every voice and metronome with `tag` at frame `at` (1 ms fade-out before it).
    StopTag { tag: u32, at: OutFrame },
    /// Master gain, applied from the next block.
    MasterGain(f32),
    /// Stop everything within 1 ms and forget scheduled sounds.
    Silence,
}

/// Event for the analysis thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixEvent {
    /// A `Play` or `Metronome` was accepted and starts exactly at `at`.
    Started { tag: u32, at: OutFrame },
    /// Beat `index` of a metronome starts at `at` (emitted in the block that contains it).
    Beat { tag: u32, index: u32, at: OutFrame },
    /// The command arrived `by` frames after its start: it plays in phase, with a 1 ms
    /// fade-in, and no `Started` is sent for it.
    Late { tag: u32, by: u32 },
    /// No free voice or metronome slot, or an invalid command.
    Rejected { tag: u32 },
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    tag: u32,
    sound: SoundId,
    start: u64,
    gain: f32,
    fade_start: u64,
    fade_len: u64,
    stop_at: u64,
}

#[derive(Clone, Copy)]
struct Track {
    active: bool,
    tag: u32,
    start: u64,
    period: f64,
    accent_every: u8,
    beats: u32,
    gain: f32,
    fade_start: u64,
    fade_len: u64,
    stop_at: u64,
}

const IDLE_VOICE: Voice = Voice {
    active: false,
    tag: 0,
    sound: SoundId::Click,
    start: 0,
    gain: 0.0,
    fade_start: 0,
    fade_len: 0,
    stop_at: NEVER,
};
const IDLE_TRACK: Track = Track {
    active: false,
    tag: 0,
    start: 0,
    period: 1.0,
    accent_every: 0,
    beats: 0,
    gain: 0.0,
    fade_start: 0,
    fade_len: 0,
    stop_at: NEVER,
};

struct Events {
    tx: rtrb::Producer<MixEvent>,
    dropped: u64,
}

impl Events {
    fn push(&mut self, e: MixEvent) {
        if self.tx.push(e).is_err() {
            self.dropped += 1;
        }
    }
}

/// The output mixer. Owned by the output callback.
pub struct Mixer {
    bank: SoundBank,
    cmds: rtrb::Consumer<MixCmd>,
    ev: Events,
    voices: [Voice; VOICES],
    tracks: [Track; TRACKS],
    master: f32,
    fade: u64,
}

/// Where and how loud to add one sound to a block.
struct Placement {
    start: u64,
    gain: f32,
    fade_start: u64,
    fade_len: u64,
    stop_at: u64,
}

fn add_sound(out: &mut [f32], channels: usize, first: u64, snd: &[f32], p: &Placement, fade: u64) {
    let end = first.saturating_add((out.len() / channels) as u64);
    let lo = first.max(p.start);
    let hi = end
        .min(p.start.saturating_add(snd.len() as u64))
        .min(p.stop_at);
    for f in lo..hi {
        let mut g = p.gain;
        if p.fade_len > 0 && f < p.fade_start.saturating_add(p.fade_len) {
            g *= (f - p.fade_start + 1) as f32 / p.fade_len as f32;
        }
        if p.stop_at != NEVER && p.stop_at - f < fade {
            g *= (p.stop_at - f) as f32 / fade as f32;
        }
        let v = snd[(f - p.start) as usize] * g;
        let base = (f - first) as usize * channels;
        for o in &mut out[base..base + channels] {
            *o += v;
        }
    }
}

fn beat_pos(t: &Track, k: u64) -> u64 {
    t.start.saturating_add((k as f64 * t.period).round() as u64)
}

impl Mixer {
    /// Builds the mixer. The command queue is drained and the event queue filled by `render`.
    pub fn new(
        bank: SoundBank,
        cmds: rtrb::Consumer<MixCmd>,
        events: rtrb::Producer<MixEvent>,
    ) -> Self {
        let fade = ((FADE_SECONDS * bank.sample_rate()).round() as u64).max(1);
        Self {
            bank,
            cmds,
            ev: Events {
                tx: events,
                dropped: 0,
            },
            voices: [IDLE_VOICE; VOICES],
            tracks: [IDLE_TRACK; TRACKS],
            master: 1.0,
            fade,
        }
    }

    /// Events dropped because the event queue was full.
    pub fn dropped_events(&self) -> u64 {
        self.ev.dropped
    }

    /// Voices and metronomes currently scheduled or sounding.
    pub fn active(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
            + self.tracks.iter().filter(|t| t.active).count()
    }

    /// Renders `out` (interleaved, `channels` per frame; every channel gets the same
    /// signal). `first` is the frame index of the first frame of `out`. No allocation.
    pub fn render(&mut self, out: &mut [f32], channels: u16, first: OutFrame) {
        out.fill(0.0);
        let ch = channels as usize;
        if ch == 0 {
            return;
        }
        let frames = (out.len() / ch) as u64;
        let end = first.saturating_add(frames);
        // Bounded: only what was queued on entry, so a busy producer cannot hold the callback.
        for _ in 0..self.cmds.slots() {
            let Ok(cmd) = self.cmds.pop() else { break };
            self.apply(cmd, first);
        }
        let master = self.master;
        let fade = self.fade;
        for v in self.voices.iter().filter(|v| v.active) {
            let p = Placement {
                start: v.start,
                gain: v.gain * master,
                fade_start: v.fade_start,
                fade_len: v.fade_len,
                stop_at: v.stop_at,
            };
            add_sound(out, ch, first, self.bank.get(v.sound), &p, fade);
        }
        for t in self.tracks.iter().filter(|t| t.active) {
            let len = self.bank.get(SoundId::Click).len() as u64;
            let mut k = if first < t.start.saturating_add(len) {
                0
            } else {
                (((first - len - t.start) as f64 / t.period).floor() as u64).saturating_sub(1)
            };
            loop {
                if t.beats > 0 && k >= t.beats as u64 {
                    break;
                }
                let pos = beat_pos(t, k);
                if pos >= end || pos >= t.stop_at {
                    break;
                }
                if pos.saturating_add(len) > first {
                    if pos >= first {
                        self.ev.push(MixEvent::Beat {
                            tag: t.tag,
                            index: k as u32,
                            at: pos,
                        });
                    }
                    let id = if t.accent_every > 0 && k % t.accent_every as u64 == 0 {
                        SoundId::Accent
                    } else {
                        SoundId::Click
                    };
                    let p = Placement {
                        start: pos,
                        gain: t.gain * master,
                        fade_start: t.fade_start,
                        fade_len: t.fade_len,
                        stop_at: t.stop_at,
                    };
                    add_sound(out, ch, first, self.bank.get(id), &p, fade);
                }
                k += 1;
            }
        }
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
        // Free what has finished.
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let done = v.start.saturating_add(self.bank.get(v.sound).len() as u64);
            if done <= end || v.stop_at <= end {
                v.active = false;
            }
        }
        let len = self.bank.get(SoundId::Click).len() as u64;
        for t in self.tracks.iter_mut().filter(|t| t.active) {
            let finished =
                t.beats > 0 && beat_pos(t, t.beats as u64 - 1).saturating_add(len) <= end;
            if finished || t.stop_at <= end {
                t.active = false;
            }
        }
    }

    fn apply(&mut self, cmd: MixCmd, first: u64) {
        let fade = self.fade;
        match cmd {
            MixCmd::Play {
                tag,
                sound,
                at,
                gain,
            } => {
                if at > MAX_FRAME {
                    self.ev.push(MixEvent::Rejected { tag });
                    return;
                }
                let len = self.bank.get(sound).len() as u64;
                let late = first.saturating_sub(at);
                if late > 0 {
                    self.ev.push(MixEvent::Late {
                        tag,
                        by: late.min(u32::MAX as u64) as u32,
                    });
                    if late >= len {
                        return;
                    }
                }
                let Some(slot) = self.voices.iter_mut().find(|v| !v.active) else {
                    self.ev.push(MixEvent::Rejected { tag });
                    return;
                };
                *slot = Voice {
                    active: true,
                    tag,
                    sound,
                    start: at,
                    gain,
                    fade_start: first,
                    fade_len: if late > 0 { fade } else { 0 },
                    stop_at: NEVER,
                };
                if late == 0 {
                    self.ev.push(MixEvent::Started { tag, at });
                }
            }
            MixCmd::Metronome {
                tag,
                start,
                period,
                accent_every,
                beats,
                gain,
            } => {
                let span = period * beats.saturating_sub(1) as f64;
                if !(period.is_finite()
                    && (1.0..=MAX_FRAME as f64).contains(&period)
                    && start <= MAX_FRAME
                    && span <= (MAX_FRAME - start) as f64)
                {
                    self.ev.push(MixEvent::Rejected { tag });
                    return;
                }
                let Some(slot) = self.tracks.iter_mut().find(|t| !t.active) else {
                    self.ev.push(MixEvent::Rejected { tag });
                    return;
                };
                let late = first.saturating_sub(start);
                *slot = Track {
                    active: true,
                    tag,
                    start,
                    period,
                    accent_every,
                    beats,
                    gain,
                    fade_start: first,
                    fade_len: if late > 0 { fade } else { 0 },
                    stop_at: NEVER,
                };
                if late > 0 {
                    self.ev.push(MixEvent::Late {
                        tag,
                        by: late.min(u32::MAX as u64) as u32,
                    });
                } else {
                    self.ev.push(MixEvent::Started { tag, at: start });
                }
            }
            MixCmd::StopTag { tag, at } => {
                if at > MAX_FRAME {
                    self.ev.push(MixEvent::Rejected { tag });
                    return;
                }
                let at = at.max(first.saturating_add(fade));
                for v in self.voices.iter_mut().filter(|v| v.active && v.tag == tag) {
                    v.stop_at = v.stop_at.min(at);
                }
                for t in self.tracks.iter_mut().filter(|t| t.active && t.tag == tag) {
                    t.stop_at = t.stop_at.min(at);
                }
            }
            MixCmd::MasterGain(g) => self.master = g,
            MixCmd::Silence => {
                let at = first.saturating_add(fade);
                for v in self.voices.iter_mut().filter(|v| v.active) {
                    v.stop_at = v.stop_at.min(at);
                }
                for t in self.tracks.iter_mut().filter(|t| t.active) {
                    t.stop_at = t.stop_at.min(at);
                }
            }
        }
    }
}
