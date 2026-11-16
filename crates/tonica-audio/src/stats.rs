//! Engine counters: lock-free atomics written by the callbacks and the analysis side, and
//! the plain [`Stats`] snapshot read from any thread.
//!
//! Everything a callback writes here is a relaxed atomic operation on a counter it alone
//! writes (or `fetch_add`): no allocation, no locks, no syscalls. The only time source a
//! callback reads is `Instant::now()` (a vDSO / `mach_absolute_time` / QPC read, no system
//! call that blocks), and only when [`EngineConfig::time_callbacks`] is on.
//!
//! [`EngineConfig::time_callbacks`]: crate::engine::EngineConfig::time_callbacks

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::clock::ClockQuality;

const R: Ordering = Ordering::Relaxed;

/// Queues of the engine, named in [`EngineEvent::Dropped`] and in [`Drops`].
///
/// [`EngineEvent::Dropped`]: crate::engine::EngineEvent::Dropped
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ring {
    /// Control thread to mixer ([`MixCmd`](crate::mixer::MixCmd)); a refused `send`.
    MixCmds,
    /// Analysis thread to the client ([`EngineEvent`](crate::engine::EngineEvent)).
    Events,
    /// Input callback to analysis (block stamps and onsets).
    InEvents,
    /// Input callback to analysis (tuner frames).
    Pitch,
    /// Output callback to analysis (stamps).
    OutStamps,
    /// Mixer to analysis ([`MixEvent`](crate::mixer::MixEvent)).
    MixEvents,
    /// Error callback to analysis.
    Backend,
    /// Raw tap samples (not frames of events: samples).
    Tap,
}

/// Items dropped because a queue was full, per queue. The new item is the one dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drops {
    /// Commands the mixer queue refused.
    pub mix_cmds: u64,
    /// Events the client queue refused.
    pub events: u64,
    /// Input block/onset items.
    pub in_events: u64,
    /// Tuner frames.
    pub pitch: u64,
    /// Output stamps.
    pub out_stamps: u64,
    /// Mixer events.
    pub mix_events: u64,
    /// Backend (error callback) events.
    pub backend: u64,
    /// Tap samples.
    pub tap: u64,
}

/// Snapshot of every counter. Cumulative over the life of the engine (reopening keeps them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    /// Epoch of the clock map (0 at start, +1 per reopen).
    pub epoch: u32,
    /// Offset between the input and output device clocks, as reported by the backend, in ns.
    /// Report only: the backend has already applied it to the input stamps.
    pub origin_offset_ns: i64,
    /// Input overruns reported by the backend.
    pub xruns_in: u64,
    /// Output underruns reported by the backend.
    pub xruns_out: u64,
    /// Clock steps found on the input stream.
    pub steps_in: u64,
    /// Clock steps found on the output stream.
    pub steps_out: u64,
    /// Dropped items per queue.
    pub drops: Drops,
    /// Non-finite input samples replaced by 0.
    pub nonfinite_in: u64,
    /// Non-finite output samples replaced by 0 (a mixer bug if ever above 0).
    pub nonfinite_out: u64,
    /// Input callbacks processed.
    pub callbacks_in: u64,
    /// Output callbacks processed.
    pub callbacks_out: u64,
    /// Input frames processed.
    pub frames_in: u64,
    /// Output frames rendered.
    pub frames_out: u64,
    /// Callbacks that ran before the stages were handed over or after the engine halted.
    pub skipped_in: u64,
    /// As `skipped_in`, for the output (which played silence).
    pub skipped_out: u64,
    /// Slowest input callback in ns; 0 when timing is off.
    pub worst_in_ns: u64,
    /// Slowest output callback in ns; 0 when timing is off.
    pub worst_out_ns: u64,
    /// Events the backend reported (all kinds).
    pub backend_events: u64,
    /// Onset events forwarded (`Heard` and `Strum`).
    pub onsets: u64,
    /// Of those, the ones flagged "not judged".
    pub onsets_unjudged: u64,
    /// Device lost, invalidated or changed: the streams are (being) closed.
    pub device_lost: bool,
    /// The calibration no longer applies (device event). Cleared by the calibration code.
    pub calibration_stale: bool,
    /// Clock map quality at the last analysis step.
    pub quality: ClockQuality,
}

/// Counters shared between the callbacks, the analysis side and the control thread.
#[derive(Default)]
pub(crate) struct Shared {
    // Control flags.
    /// Callbacks play silence / ignore input (set on a fatal device event).
    pub halted: AtomicBool,
    pub lost: AtomicBool,
    pub calib_stale: AtomicBool,
    pub epoch: AtomicU32,
    pub origin_offset_ns: AtomicI64,
    pub quality: AtomicU8,
    /// End (first + frames) of the last block of each stream, for ordering error events.
    pub in_end: AtomicU64,
    pub out_end: AtomicU64,
    // Callbacks.
    pub cb_in: AtomicU64,
    pub cb_out: AtomicU64,
    pub frames_in: AtomicU64,
    pub frames_out: AtomicU64,
    pub skipped_in: AtomicU64,
    pub skipped_out: AtomicU64,
    pub worst_in_ns: AtomicU64,
    pub worst_out_ns: AtomicU64,
    pub nonfinite_in: AtomicU64,
    pub nonfinite_out: AtomicU64,
    pub xruns_in: AtomicU64,
    pub xruns_out: AtomicU64,
    pub backend_events: AtomicU64,
    // Drops.
    pub drop_cmds: AtomicU64,
    pub drop_events: AtomicU64,
    pub drop_in_events: AtomicU64,
    /// Mirrored from the capture stage by the input callback (single writer).
    pub drop_pitch: AtomicU64,
    /// Counted by the analysis side (pending queue overflow, unplaceable frames).
    pub drop_pitch_an: AtomicU64,
    /// Latched fatal device event (0 none, 1 Invalidated, 2 DeviceLost, 3 DeviceChanged).
    pub fatal: AtomicU8,
    pub drop_out_stamps: AtomicU64,
    pub drop_mix_events: AtomicU64,
    pub drop_backend: AtomicU64,
    pub drop_tap: AtomicU64,
    // Analysis.
    pub steps_in: AtomicU64,
    pub steps_out: AtomicU64,
    pub onsets: AtomicU64,
    pub onsets_unjudged: AtomicU64,
}

pub(crate) fn quality_code(q: ClockQuality) -> u8 {
    match q {
        ClockQuality::Acquiring => 0,
        ClockQuality::Locked => 1,
        ClockQuality::Degraded => 2,
        ClockQuality::Broken => 3,
    }
}

fn quality_of(code: u8) -> ClockQuality {
    match code {
        1 => ClockQuality::Locked,
        2 => ClockQuality::Degraded,
        3 => ClockQuality::Broken,
        _ => ClockQuality::Acquiring,
    }
}

/// `max` for a counter with a single writer (a plain load/store: no read-modify-write).
pub(crate) fn store_max(a: &AtomicU64, v: u64) {
    if v > a.load(R) {
        a.store(v, R);
    }
}

impl Shared {
    pub(crate) fn ring_drops(&self, ring: Ring) -> u64 {
        match ring {
            Ring::MixCmds => self.drop_cmds.load(R),
            Ring::Events => self.drop_events.load(R),
            Ring::InEvents => self.drop_in_events.load(R),
            Ring::Pitch => self
                .drop_pitch
                .load(R)
                .wrapping_add(self.drop_pitch_an.load(R)),
            Ring::OutStamps => self.drop_out_stamps.load(R),
            Ring::MixEvents => self.drop_mix_events.load(R),
            Ring::Backend => self.drop_backend.load(R),
            Ring::Tap => self.drop_tap.load(R),
        }
    }

    pub(crate) fn snapshot(&self) -> Stats {
        Stats {
            epoch: self.epoch.load(R),
            origin_offset_ns: self.origin_offset_ns.load(R),
            xruns_in: self.xruns_in.load(R),
            xruns_out: self.xruns_out.load(R),
            steps_in: self.steps_in.load(R),
            steps_out: self.steps_out.load(R),
            drops: Drops {
                mix_cmds: self.drop_cmds.load(R),
                events: self.drop_events.load(R),
                in_events: self.drop_in_events.load(R),
                pitch: self
                    .drop_pitch
                    .load(R)
                    .wrapping_add(self.drop_pitch_an.load(R)),
                out_stamps: self.drop_out_stamps.load(R),
                mix_events: self.drop_mix_events.load(R),
                backend: self.drop_backend.load(R),
                tap: self.drop_tap.load(R),
            },
            nonfinite_in: self.nonfinite_in.load(R),
            nonfinite_out: self.nonfinite_out.load(R),
            callbacks_in: self.cb_in.load(R),
            callbacks_out: self.cb_out.load(R),
            frames_in: self.frames_in.load(R),
            frames_out: self.frames_out.load(R),
            skipped_in: self.skipped_in.load(R),
            skipped_out: self.skipped_out.load(R),
            worst_in_ns: self.worst_in_ns.load(R),
            worst_out_ns: self.worst_out_ns.load(R),
            backend_events: self.backend_events.load(R),
            onsets: self.onsets.load(R),
            onsets_unjudged: self.onsets_unjudged.load(R),
            device_lost: self.lost.load(R),
            calibration_stale: self.calib_stale.load(R),
            quality: quality_of(self.quality.load(R)),
        }
    }
}
