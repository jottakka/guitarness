//! Input-callback analysis stage: downmix to mono, onset detector, tuner, raw tap.
//!
//! [`CaptureStage::process`] runs on the real-time input thread: no allocation, no locks,
//! no syscalls. Everything it produces goes through `rtrb` rings and atomic counters. A full
//! ring drops the new item and counts it; nothing ever blocks.
//!
//! # Time mapping (onset sample time to [`InFrame`])
//!
//! The detector counts mono frames since the stage was built (or last [`reset`]): detector
//! time 0 is the first frame the stage ever processed. Onset times (`Strum::t`, `Heard::t`,
//! `Heard::at`) are `f64` in that unit, and may be earlier than the block that emitted them
//! (retro-dating).
//!
//! Every callback first pushes [`InEvent::Block`] `{ stamp, det_first }`, where `det_first`
//! is the detector time of the block's first frame. Onsets emitted while processing that
//! block follow it in the same ring. A consumer keeps the latest `Block` and maps
//!
//! ```text
//! in_frame = block.stamp.first as f64 + (t - block.det_first as f64)
//! ```
//!
//! This stays exact across input gaps (a jump in `stamp.first` just changes the next
//! block's offset) and across dropped blocks, as long as the `Block` event itself arrived.
//! If the `Block` push fails (ring full), the onsets found in that callback are dropped
//! too (counted in `dropped_in`), so an `Onset` in the ring always follows its own `Block`.
//! A lost block shows as a jump of `det_first` in the next `Block`.
//! Pitch frames ([`guitarness_dsp::Frame`]) carry `t` in seconds on the same detector clock
//! (`t * fs_in` = detector time); map them the same way.
//!
//! [`reset`]: CaptureStage::reset
//!
//! # Raw ring (for the tape)
//!
//! Separate from the 2 s tap (which belongs to the calibrator), the raw ring carries
//! whole blocks for the tape writer. With [`CaptureOutputs::set_raw`]`(true)`, each chunk
//! the stage processes (at most `max_block` frames; larger callbacks are split) pushes its
//! cleaned mono samples (the very samples the detector sees, NaN/Inf already 0) and then a
//! [`RawMark`] `{ in_first, det_first, frames }`:
//!
//! - `in_first` = `stamp.first` + frames of this callback already processed (an `InFrame`);
//! - `det_first` = detector time of the chunk's first frame;
//! - `frames` = samples in the chunk.
//!
//! The chunk is all or nothing: if the sample ring lacks room for all `frames` samples or
//! the mark ring has no slot, the whole chunk is dropped and counted (`dropped_blocks`), so a
//! consumer never sees a partial block. A drop shows as a jump of `det_first`; a gap in
//! the input shows as a jump of `in_first`. Switched off (the default) it has no effect.
//!
//! API that engine unit A1 wires into `TapHandle::{set_raw, take_raw}`:
//!
//! - [`RAW_SECONDS`], [`RAW_MARK_CAP`], [`RawMark`], [`RawOutput`]
//!   (`read_block`, `dropped_blocks`; `Send`);
//! - [`CaptureOutputs::set_raw`] (`&self`, atomic), [`CaptureOutputs::take_raw`]
//!   (`&mut self`, once), [`CaptureOutputs::raw_dropped_blocks`].
//!
//! The single consumer is the tape thread. `CaptureStats` is unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use guitarness_dsp::{AcfMode, Frame, OnsetConfig, OnsetDetector, OnsetEvent, Pipeline};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::types::{ChannelPick, Stamp};

/// Seconds of raw audio the tap ring holds.
pub const TAP_SECONDS: f64 = 2.0;
/// Seconds of raw audio the raw ring holds.
pub const RAW_SECONDS: f64 = 4.0;
/// Capacity of the raw [`RawMark`] ring (one mark per processed chunk).
pub const RAW_MARK_CAP: usize = 8192;
/// Default capacity of the [`InEvent`] ring.
pub const IN_RING_CAP: usize = 4096;
/// Default capacity of the pitch ring.
pub const PITCH_RING_CAP: usize = 512;

/// What the input callback tells the analysis thread. `Copy`, small.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InEvent {
    /// One input callback. Precedes the onsets found while processing it.
    Block {
        /// Stamp of the callback.
        stamp: Stamp,
        /// Detector time (mono frames since start/reset) of the first frame of the block.
        det_first: u64,
    },
    /// An onset event; times are detector time (see the module docs).
    Onset(OnsetEvent),
}

/// Address of one block in the raw ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawMark {
    /// Input frame (`InFrame`) of the block's first sample.
    pub in_first: u64,
    /// Detector time of the block's first sample.
    pub det_first: u64,
    /// Samples in the block.
    pub frames: u32,
}

/// Counters shared between the callback and the rest of the program.
#[derive(Debug, Default)]
struct Shared {
    blocks: AtomicU64,
    frames: AtomicU64,
    dropped_in: AtomicU64,
    dropped_pitch: AtomicU64,
    tap_dropped: AtomicU64,
    raw_dropped_blocks: AtomicU64,
    raw_on: AtomicBool,
    nonfinite: AtomicU64,
    tap_on: AtomicBool,
    /// Detector time of the first tap sample after the tap was switched on.
    tap_origin: AtomicU64,
}

/// Snapshot of the capture counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureStats {
    /// Callbacks processed.
    pub blocks: u64,
    /// Frames processed.
    pub frames: u64,
    /// [`InEvent`]s dropped because the ring was full.
    pub dropped_in: u64,
    /// Pitch frames dropped because the ring was full.
    pub dropped_pitch: u64,
    /// Tap samples dropped because the ring was full.
    pub tap_dropped: u64,
    /// NaN/Inf input samples replaced by 0.
    pub nonfinite: u64,
}

impl Shared {
    fn snapshot(&self) -> CaptureStats {
        CaptureStats {
            blocks: self.blocks.load(Ordering::Relaxed),
            frames: self.frames.load(Ordering::Relaxed),
            dropped_in: self.dropped_in.load(Ordering::Relaxed),
            dropped_pitch: self.dropped_pitch.load(Ordering::Relaxed),
            tap_dropped: self.tap_dropped.load(Ordering::Relaxed),
            nonfinite: self.nonfinite.load(Ordering::Relaxed),
        }
    }
}

/// Producer side of the capture rings, owned by the [`CaptureStage`].
pub struct CaptureRings {
    events: Producer<InEvent>,
    pitch: Producer<Frame>,
    tap: Producer<f32>,
    raw: Producer<f32>,
    raw_marks: Producer<RawMark>,
    shared: Arc<Shared>,
}

/// Consumer side, for the analysis thread.
pub struct CaptureOutputs {
    /// Block stamps and onset events, in order.
    pub events: Consumer<InEvent>,
    /// Tuner frames (time in seconds on the detector clock).
    pub pitch: Consumer<Frame>,
    /// Raw mono tap (f32), only filled while [`CaptureOutputs::set_tap`] is on.
    pub tap: Consumer<f32>,
    raw: Option<RawOutput>,
    shared: Arc<Shared>,
}

/// Consumer side of the raw ring: whole blocks or nothing. `Send`.
pub struct RawOutput {
    samples: Consumer<f32>,
    marks: Consumer<RawMark>,
    shared: Arc<Shared>,
}

impl RawOutput {
    /// Take the oldest whole block: `dst` is cleared and filled with its samples, and its
    /// mark is returned. `None` (and `dst` untouched) when no block is ready.
    pub fn read_block(&mut self, dst: &mut Vec<f32>) -> Option<RawMark> {
        let mark = self.marks.pop().ok()?;
        let n = mark.frames as usize;
        // The producer writes the samples before the mark, so they are all here.
        let chunk = self.samples.read_chunk(n).ok()?;
        let (a, b) = chunk.as_slices();
        dst.clear();
        dst.extend_from_slice(a);
        dst.extend_from_slice(b);
        chunk.commit_all();
        Some(mark)
    }

    /// Blocks dropped because the sample or mark ring was full.
    pub fn dropped_blocks(&self) -> u64 {
        self.shared.raw_dropped_blocks.load(Ordering::Relaxed)
    }
}

impl CaptureRings {
    /// Rings with default capacities. Allocates; call before the stream starts.
    pub fn new(fs_in: f64) -> (CaptureRings, CaptureOutputs) {
        Self::with_capacity(fs_in, IN_RING_CAP, PITCH_RING_CAP)
    }

    /// Rings with explicit capacities (items). The tap holds [`TAP_SECONDS`] at `fs_in`.
    pub fn with_capacity(
        fs_in: f64,
        in_cap: usize,
        pitch_cap: usize,
    ) -> (CaptureRings, CaptureOutputs) {
        let (events, events_rx) = RingBuffer::new(in_cap.max(1));
        let (pitch, pitch_rx) = RingBuffer::new(pitch_cap.max(1));
        let tap_cap = (fs_in * TAP_SECONDS).round().max(1.0) as usize;
        let (tap, tap_rx) = RingBuffer::new(tap_cap);
        let raw_cap = (fs_in * RAW_SECONDS).round().max(1.0) as usize;
        let (raw, raw_rx) = RingBuffer::new(raw_cap);
        let (raw_marks, raw_marks_rx) = RingBuffer::new(RAW_MARK_CAP);
        let shared = Arc::new(Shared::default());
        (
            CaptureRings {
                events,
                pitch,
                tap,
                raw,
                raw_marks,
                shared: shared.clone(),
            },
            CaptureOutputs {
                events: events_rx,
                pitch: pitch_rx,
                tap: tap_rx,
                raw: Some(RawOutput {
                    samples: raw_rx,
                    marks: raw_marks_rx,
                    shared: shared.clone(),
                }),
                shared,
            },
        )
    }
}

impl CaptureOutputs {
    /// Counters snapshot.
    pub fn stats(&self) -> CaptureStats {
        self.shared.snapshot()
    }

    /// Start or stop filling the tap (off by default). While on, the tap receives every mono
    /// sample; if the consumer falls behind by more than 2 s the new samples are dropped
    /// (counted in `tap_dropped`), so the tap is a gapless run from [`tap_origin`] unless
    /// `tap_dropped` grew.
    ///
    /// [`tap_origin`]: CaptureOutputs::tap_origin
    pub fn set_tap(&self, on: bool) {
        self.shared.tap_on.store(on, Ordering::Release);
    }

    /// Start or stop filling the raw ring (off by default; see the module docs).
    pub fn set_raw(&self, on: bool) {
        self.shared.raw_on.store(on, Ordering::Release);
    }

    /// Hand out the raw ring consumer. Returns it once; `None` afterwards.
    pub fn take_raw(&mut self) -> Option<RawOutput> {
        self.raw.take()
    }

    /// Raw blocks dropped so far (same counter as [`RawOutput::dropped_blocks`]).
    pub fn raw_dropped_blocks(&self) -> u64 {
        self.shared.raw_dropped_blocks.load(Ordering::Relaxed)
    }

    /// Detector time of the first tap sample since the tap was last switched on (valid once
    /// the first sample has arrived).
    pub fn tap_origin(&self) -> u64 {
        self.shared.tap_origin.load(Ordering::Acquire)
    }
}

/// The input-callback analysis stage.
pub struct CaptureStage {
    ch: usize,
    pick: ChannelPick,
    max_block: usize,
    fs_in: f64,
    cfg: OnsetConfig,
    onset: OnsetDetector,
    pipe: Pipeline,
    mono: Vec<f32>,
    /// Detector time of the next frame to process.
    pos: u64,
    rings: CaptureRings,
    tap_was_on: bool,
    /// The last `Block` event was published (so onsets that follow it have their block).
    block_ok: bool,
}

impl CaptureStage {
    /// Build the stage with the default onset configuration. Allocates.
    ///
    /// `ch`: interleaved channel count of the input. `max_block`: frames per processing
    /// chunk (larger callbacks are processed in chunks). An out-of-range
    /// `ChannelPick::Index` is clamped to the last channel.
    pub fn new(
        fs_in: f64,
        ch: u16,
        pick: ChannelPick,
        max_block: usize,
        rings: CaptureRings,
    ) -> Self {
        Self::with_config(fs_in, ch, pick, max_block, rings, OnsetConfig::default())
    }

    /// As [`new`](Self::new) with an explicit onset configuration.
    pub fn with_config(
        fs_in: f64,
        ch: u16,
        pick: ChannelPick,
        max_block: usize,
        rings: CaptureRings,
        cfg: OnsetConfig,
    ) -> Self {
        let max_block = max_block.max(1);
        Self {
            ch: usize::from(ch.max(1)),
            pick,
            max_block,
            fs_in,
            onset: OnsetDetector::new(fs_in, cfg.clone()),
            cfg,
            pipe: Pipeline::demo(fs_in, AcfMode::Fft, max_block),
            mono: vec![0.0; max_block],
            pos: 0,
            rings,
            tap_was_on: false,
            block_ok: true,
        }
    }

    /// The tuner, for configuration (targets, A4) before the stage moves into the callback.
    pub fn pipeline_mut(&mut self) -> &mut Pipeline {
        &mut self.pipe
    }

    /// Counters snapshot.
    pub fn stats(&self) -> CaptureStats {
        self.rings.shared.snapshot()
    }

    /// Start a new epoch: detector time returns to 0 and the detector and tuner restart.
    /// **Allocates**: call it outside the callback (before the stream starts). Counters and
    /// ring contents are kept.
    pub fn reset(&mut self) {
        self.onset = OnsetDetector::new(self.fs_in, self.cfg.clone());
        self.pipe = Pipeline::demo(self.fs_in, AcfMode::Fft, self.max_block);
        self.pos = 0;
        self.tap_was_on = false;
        self.block_ok = true;
    }

    /// Close any open onset group (end of stream) and push the resulting events.
    pub fn flush(&mut self) {
        let events = &mut self.rings.events;
        let sh = &*self.rings.shared;
        let block_ok = self.block_ok;
        self.onset.flush(|e| {
            if !block_ok || events.push(InEvent::Onset(e)).is_err() {
                sh.dropped_in.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    /// Process one input callback. `interleaved` holds `frames * ch` samples (a trailing
    /// partial frame is ignored). Real-time safe.
    pub fn process(&mut self, interleaved: &[f32], stamp: Stamp) {
        let ch = self.ch;
        let frames = interleaved.len() / ch;
        let sh = &self.rings.shared;
        sh.blocks.fetch_add(1, Ordering::Relaxed);
        sh.frames.fetch_add(frames as u64, Ordering::Relaxed);

        let ev = InEvent::Block {
            stamp,
            det_first: self.pos,
        };
        self.block_ok = self.rings.events.push(ev).is_ok();
        if !self.block_ok {
            sh.dropped_in.fetch_add(1, Ordering::Relaxed);
        }
        let block_ok = self.block_ok;

        let tap_on = sh.tap_on.load(Ordering::Acquire);
        if tap_on && !self.tap_was_on {
            sh.tap_origin.store(self.pos, Ordering::Release);
        }
        self.tap_was_on = tap_on;
        let raw_on = sh.raw_on.load(Ordering::Acquire);

        let mut bad = 0u64;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(self.max_block);
            let src = &interleaved[done * ch..(done + n) * ch];
            let mono = &mut self.mono[..n];
            bad += downmix(src, ch, self.pick, mono);

            let events = &mut self.rings.events;
            let sh = &*self.rings.shared;
            self.onset.process(mono, |e| {
                // An onset is published only behind a published `Block` event.
                if !block_ok || events.push(InEvent::Onset(e)).is_err() {
                    sh.dropped_in.fetch_add(1, Ordering::Relaxed);
                }
            });

            let pitch = &mut self.rings.pitch;
            let t0 = self.pos as f64 / self.fs_in;
            self.pipe.process(
                mono,
                t0,
                |f| {
                    if pitch.push(*f).is_err() {
                        sh.dropped_pitch.fetch_add(1, Ordering::Relaxed);
                    }
                },
                |_| {},
            );

            if tap_on {
                let tap = &mut self.rings.tap;
                let room = tap.slots().min(n);
                if let Ok(mut chunk) = tap.write_chunk_uninit(room) {
                    let (a, b) = chunk.as_mut_slices();
                    let (m1, m2) = mono[..room].split_at(a.len());
                    for (d, s) in a.iter_mut().zip(m1) {
                        d.write(*s);
                    }
                    for (d, s) in b.iter_mut().zip(m2) {
                        d.write(*s);
                    }
                    // SAFETY: all `room` slots were initialised above.
                    unsafe { chunk.commit_all() };
                }
                if room < n {
                    sh.tap_dropped
                        .fetch_add((n - room) as u64, Ordering::Relaxed);
                }
            }

            if raw_on {
                let raw = &mut self.rings.raw;
                let sh = &*self.rings.shared;
                // All or nothing: samples and mark must both fit, or the chunk is dropped.
                let fits = raw.slots() >= n && self.rings.raw_marks.slots() >= 1;
                let mut ok = false;
                if fits && let Ok(mut chunk) = raw.write_chunk_uninit(n) {
                    let (a, b) = chunk.as_mut_slices();
                    let (m1, m2) = mono.split_at(a.len());
                    for (d, s) in a.iter_mut().zip(m1) {
                        d.write(*s);
                    }
                    for (d, s) in b.iter_mut().zip(m2) {
                        d.write(*s);
                    }
                    // SAFETY: all `n` slots were initialised above.
                    unsafe { chunk.commit_all() };
                    let mark = RawMark {
                        in_first: stamp.first + done as u64,
                        det_first: self.pos,
                        frames: n as u32,
                    };
                    ok = self.rings.raw_marks.push(mark).is_ok();
                }
                if !ok {
                    sh.raw_dropped_blocks.fetch_add(1, Ordering::Relaxed);
                }
            }

            self.pos += n as u64;
            done += n;
        }
        if bad > 0 {
            self.rings
                .shared
                .nonfinite
                .fetch_add(bad, Ordering::Relaxed);
        }
    }
}

/// Interleaved to mono; non-finite samples become 0. Returns how many were replaced.
fn downmix(src: &[f32], ch: usize, pick: ChannelPick, out: &mut [f32]) -> u64 {
    let mut bad = 0u64;
    let mut clean = |x: f32| {
        if x.is_finite() {
            x
        } else {
            bad += 1;
            0.0
        }
    };
    match pick {
        ChannelPick::Index(i) => {
            let i = usize::from(i).min(ch - 1);
            for (o, frame) in out.iter_mut().zip(src.chunks_exact(ch)) {
                *o = clean(frame[i]);
            }
        }
        ChannelPick::Mean => {
            let inv = 1.0 / ch as f32;
            for (o, frame) in out.iter_mut().zip(src.chunks_exact(ch)) {
                let mut s = 0.0f32;
                for &x in frame {
                    s += clean(x);
                }
                *o = s * inv;
            }
        }
    }
    bad
}
