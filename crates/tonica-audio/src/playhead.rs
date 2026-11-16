//! The playhead: which output frame is sounding now, for drawing.
//!
//! Two pieces, both free of locks:
//!
//! - [`Playhead`]: the stamp of the last output block, published by the output callback
//!   in atomics and readable from any thread ([`Playhead::last_out`]).
//! - [`PlayClock`]: turns the host clock (`HostClock::host_now_ns`), the latest
//!   [`ClockSnapshot`] and that stamp into an output frame that only moves forward.
//!
//! # What "now" means
//!
//! The answer is as good as the stamps of the output stream:
//!
//! - With device stamps ([`StampSource::Device`]) the frame is the one the system says is
//!   leaving the converter now.
//! - With callback stamps ([`StampSource::Callback`], the fallback) it is the frame being
//!   *requested* now. It runs ahead of the sound by the output latency, which nobody
//!   knows.
//!
//! The latency of the display is unknown too. So the drawing can sit some tens of
//! milliseconds away from the click. **Nothing here is used to judge the player**: the
//! judge compares onsets and clicks in audio frames, from the engine events alone. The
//! game keeps a `video_offset_ms` for the eye.
//!
//! # The seqlock
//!
//! The output callback is the only writer. It makes the sequence counter odd, stores the
//! fields, and makes the counter even again. A reader copies the fields between two reads
//! of the counter and keeps the copy only if both reads gave the same even number; it
//! tries up to [`READ_TRIES`] times. Every field is an atomic, so a torn read is only a
//! wrong combination of values, never undefined behaviour, and the counter catches it.
//! All accesses are `SeqCst`: the argument needs the two counter reads and the field
//! reads of the reader to be ordered against the stores of the writer, and the cost (six
//! stores per output block) does not matter.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::clock::{ClockQuality, ClockSnapshot, StampSource};
use crate::types::Side;

const SC: Ordering = Ordering::SeqCst;

/// How many times [`Playhead::last_out`] tries to get a consistent copy.
pub const READ_TRIES: usize = 8;

/// Without a new output block for this long the playhead holds, seconds.
pub const STALE_S: f64 = 0.5;

/// The stamp of one output block, as the output callback saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutMark {
    /// Output frame of the first frame of the block.
    pub first: u64,
    /// Frames in the block.
    pub frames: u32,
    /// Host time at which `first` plays, ns (`Stamp::device_ns`).
    pub device_ns: u64,
    /// Host time of the callback, ns (`Stamp::callback_ns`).
    pub callback_ns: u64,
    /// Engine epoch of the streams that produced the block.
    pub epoch: u32,
}

#[derive(Default)]
struct Cell {
    /// 0: nothing published. Odd: a write is in progress.
    seq: AtomicU64,
    first: AtomicU64,
    /// `frames` in the low half, `epoch` in the high half.
    meta: AtomicU64,
    device_ns: AtomicU64,
    callback_ns: AtomicU64,
    /// Epoch of the engine (set by the control thread on a reopen).
    epoch: AtomicU32,
}

/// The last output stamp, shared between the output callback and its readers. `Send` and
/// `Sync`; clones share the same cell. Reading never blocks and never allocates.
#[derive(Clone, Default)]
pub struct Playhead {
    cell: Arc<Cell>,
}

impl Playhead {
    /// A playhead with nothing published, at epoch 0.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Publishes the stamp of a block. One writer at a time: the output callback of the
    /// open streams. Six atomic stores; no allocation, no lock.
    pub(crate) fn publish(&self, m: OutMark) {
        let c = &*self.cell;
        // `| 1` rather than `+ 1`: a writer that died half way leaves the counter odd, and
        // the next one must not make it even while it writes.
        let odd = c.seq.load(SC) | 1;
        c.seq.store(odd, SC);
        c.first.store(m.first, SC);
        c.meta
            .store(u64::from(m.frames) | (u64::from(m.epoch) << 32), SC);
        c.device_ns.store(m.device_ns, SC);
        c.callback_ns.store(m.callback_ns, SC);
        c.seq.store(odd.wrapping_add(1), SC);
    }

    /// The engine opened new streams: stamps of older epochs are no longer returned.
    pub(crate) fn set_epoch(&self, epoch: u32) {
        self.cell.epoch.store(epoch, SC);
    }

    /// The stamp of the last output block of the current epoch. `None` before the first
    /// block of the epoch, and in the (practically impossible) case that
    /// [`READ_TRIES`] reads in a row met a write.
    ///
    /// The stamp stays after the streams stop, until the engine opens new ones: compare
    /// `callback_ns` with the host clock to know how old it is ([`PlayClock`] does).
    pub fn last_out(&self) -> Option<OutMark> {
        let c = &*self.cell;
        for _ in 0..READ_TRIES {
            let s1 = c.seq.load(SC);
            if s1 == 0 {
                return None;
            }
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let first = c.first.load(SC);
            let meta = c.meta.load(SC);
            let device_ns = c.device_ns.load(SC);
            let callback_ns = c.callback_ns.load(SC);
            if c.seq.load(SC) != s1 {
                continue;
            }
            let m = OutMark {
                first,
                frames: meta as u32,
                device_ns,
                callback_ns,
                epoch: (meta >> 32) as u32,
            };
            return (m.epoch == c.epoch.load(SC)).then_some(m);
        }
        None
    }

    /// Epoch of the engine: 0 at start, +1 per successful reopen.
    pub fn epoch(&self) -> u32 {
        self.cell.epoch.load(SC)
    }
}

/// Where a [`PlayPos`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayMode {
    /// The line of a `Locked` clock snapshot.
    Line,
    /// The last output stamp, extrapolated at the nominal rate (before the clock locks).
    Stamp,
    /// Nothing usable: the position is frozen (0 if there never was one).
    Hold,
}

/// The output frame sounding at an instant (see the module documentation for the limits).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayPos {
    /// Output frame, fractional. Never negative: 0 until frame 0 of the streams sounds.
    pub out_frame: f64,
    /// How it was obtained.
    pub mode: PlayMode,
    /// Engine epoch the frame belongs to.
    pub epoch: u32,
}

/// Picks the best source for "the output frame sounding now" and keeps the answer
/// monotonic. Owned by one thread (the one that draws). Not used for judging.
///
/// Sources, in order:
///
/// 1. [`PlayMode::Line`]: the output line of the latest snapshot, if that snapshot is
///    `Locked`, belongs to the current epoch and has seen an output stamp in the last
///    [`STALE_S`].
/// 2. [`PlayMode::Stamp`]: the last output stamp, if it is that recent, extrapolated at
///    the nominal rate.
/// 3. [`PlayMode::Hold`]: the last position shown.
///
/// Inside one epoch the position never goes back: when the source says an earlier frame
/// (an output underrun, a change of source) the position waits for it. It jumps only when
/// the epoch changes (the streams were reopened and the frame counter restarted).
#[derive(Clone, Debug)]
pub struct PlayClock {
    fs_out: f64,
    snap: Option<ClockSnapshot>,
    epoch: u32,
    shown: Option<f64>,
}

impl PlayClock {
    /// `fs_out`: nominal output rate in Hz. A snapshot of another rate (after a reopen)
    /// replaces it.
    pub fn new(fs_out: f64) -> Self {
        Self {
            fs_out,
            snap: None,
            epoch: 0,
            shown: None,
        }
    }

    /// Feeds a clock snapshot (every `EngineEvent::Clock`). Snapshots of an epoch older
    /// than the newest one seen are ignored.
    pub fn on_snapshot(&mut self, s: ClockSnapshot) {
        if self.snap.is_none_or(|old| s.epoch() >= old.epoch()) {
            self.fs_out = s.parts().fs_out;
            self.snap = Some(s);
        }
    }

    /// The output frame sounding at host time `host_now_ns`. `mark` is
    /// [`Playhead::last_out`] read just before (pass `None` if there is no playhead: the
    /// snapshots alone are used).
    pub fn frame_at(&mut self, host_now_ns: u64, mark: Option<OutMark>) -> PlayPos {
        // Epochs only grow: the newest one named by either source is the current one.
        let epoch = [mark.map(|m| m.epoch), self.snap.map(|s| s.epoch())]
            .into_iter()
            .flatten()
            .fold(self.epoch, u32::max);
        if epoch != self.epoch {
            self.epoch = epoch;
            self.shown = None;
        }
        let mark = mark.filter(|m| m.epoch == epoch);
        let snap = self.snap.filter(|s| s.epoch() == epoch);
        let mark_age_s = mark.map(|m| (host_now_ns as f64 - m.callback_ns as f64) * 1.0e-9);

        let line = snap
            .filter(|s| s.quality() == ClockQuality::Locked)
            .filter(|s| {
                // Fresh by the stamp if there is one, else by the snapshot itself.
                mark_age_s
                    .or_else(|| s.out_age_s(host_now_ns))
                    .is_some_and(|age| age <= STALE_S)
            })
            .and_then(|s| s.out_frame_at(host_now_ns))
            .map(|f| (f, PlayMode::Line));
        let stamp = || {
            let m = mark?;
            if mark_age_s? > STALE_S {
                return None;
            }
            // The same meaning as the line the map will give: the time of the sound with
            // device stamps, the time of the request once the map fell back.
            let callback = snap.is_some_and(|s| s.source(Side::Out) == StampSource::Callback);
            let t_ns = if callback { m.callback_ns } else { m.device_ns };
            let dt_s = (host_now_ns as f64 - t_ns as f64) * 1.0e-9;
            Some((m.first as f64 + dt_s * self.fs_out, PlayMode::Stamp))
        };
        let Some((mut f, mode)) = line.or_else(stamp).filter(|(f, _)| f.is_finite()) else {
            return PlayPos {
                out_frame: self.shown.unwrap_or(0.0),
                mode: PlayMode::Hold,
                epoch,
            };
        };
        // A frame cannot sound before it was rendered, and nothing sounds before frame 0
        // (the first block is requested ahead of its sound).
        if let Some(m) = mark {
            f = f.min((m.first + u64::from(m.frames)) as f64);
        }
        f = f.max(0.0);
        let f = self.shown.map_or(f, |shown| shown.max(f));
        self.shown = Some(f);
        PlayPos {
            out_frame: f,
            mode,
            epoch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A mark whose fields are all functions of `k`: any mix of two marks is detectable.
    fn mark(k: u64) -> OutMark {
        OutMark {
            first: k * 480,
            frames: (k % 1000) as u32 + 1,
            device_ns: k.wrapping_mul(10_000_019),
            callback_ns: k.wrapping_mul(9_999_991) ^ 0x5555,
            epoch: 0,
        }
    }

    #[test]
    fn nothing_published_reads_none() {
        let p = Playhead::new();
        assert_eq!(p.last_out(), None);
        assert_eq!(p.epoch(), 0);
        p.publish(mark(7));
        assert_eq!(p.last_out(), Some(mark(7)));
        p.publish(mark(8));
        assert_eq!(p.clone().last_out(), Some(mark(8)));
    }

    #[test]
    fn marks_of_another_epoch_are_not_returned() {
        let p = Playhead::new();
        p.publish(mark(3));
        p.set_epoch(1);
        assert_eq!(p.epoch(), 1);
        assert_eq!(p.last_out(), None, "stale epoch");
        let m = OutMark {
            epoch: 1,
            ..mark(4)
        };
        p.publish(m);
        assert_eq!(p.last_out(), Some(m));
    }

    #[test]
    fn a_writer_that_died_mid_write_does_not_break_the_next_one() {
        let p = Playhead::new();
        p.publish(mark(1));
        // Half a write: the counter is left odd.
        p.cell.seq.store(p.cell.seq.load(SC) | 1, SC);
        p.cell.first.store(123, SC);
        assert_eq!(p.last_out(), None, "a write in progress is never read");
        p.publish(mark(2));
        assert_eq!(p.last_out(), Some(mark(2)));
        assert_eq!(p.cell.seq.load(SC) & 1, 0);
    }

    /// Check 1 of the plan: a writer publishing as fast as it can, 10^6 reads from two
    /// reader threads, not one torn mark.
    #[test]
    fn seqlock_never_returns_a_torn_mark() {
        const READS: u64 = 1_000_000;
        let p = Playhead::new();
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (p, stop) = (p.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut k = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    k += 1;
                    p.publish(mark(k));
                }
                k
            })
        };
        // The readers start once the writer runs (on a loaded machine it may be scheduled
        // after a million reads are already done).
        while p.last_out().is_none() {
            std::thread::yield_now();
        }
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || {
                    let (mut got, mut last, mut changes) = (0u64, 0u64, 0u64);
                    for _ in 0..READS / 2 {
                        let Some(m) = p.last_out() else {
                            continue;
                        };
                        let k = m.first / 480;
                        assert_eq!(m, mark(k), "torn mark");
                        assert!(k >= last, "the mark went back: {k} after {last}");
                        changes += u64::from(k != last);
                        last = k;
                        got += 1;
                    }
                    (got, changes)
                })
            })
            .collect();
        let mut got = 0;
        let mut changes = 0;
        for r in readers {
            let (g, c) = r.join().unwrap();
            got += g;
            changes += c;
        }
        stop.store(true, Ordering::Relaxed);
        let written = writer.join().unwrap();
        println!(
            "seqlock: {READS} reads, {got} consistent, {changes} saw a new mark; {written} marks written"
        );
        // Not vacuous: the writer really ran against the readers.
        assert!(written > 10_000, "{written} marks written");
        assert!(got > READS / 10, "{got} consistent reads");
        assert!(changes > 1_000, "{changes} reads saw a new mark");
    }

    fn snapshot(quality: ClockQuality, epoch: u32, src_out: StampSource) -> ClockSnapshot {
        use crate::clock::ClockParts;
        // Output frame f plays at 1 s + f/48000 on the host clock (base 5 s).
        ClockSnapshot::from_parts(ClockParts {
            epoch,
            quality,
            n_in: 0.0,
            t_in: 0.0,
            p_in: 1.0 / 48_000.0,
            n_out: 48_000.0,
            t_out: 2.0,
            p_out: 1.0 / 48_000.0,
            fs_in: 48_000.0,
            fs_out: 48_000.0,
            src_in: StampSource::Device,
            src_out,
            base_ns: Some(5_000_000_000),
        })
    }

    const S: u64 = 1_000_000_000;

    #[test]
    fn play_clock_picks_line_then_stamp_then_hold() {
        let mut pc = PlayClock::new(48_000.0);
        // Nothing known.
        let p = pc.frame_at(7 * S, None);
        assert_eq!((p.out_frame, p.mode, p.epoch), (0.0, PlayMode::Hold, 0));

        // A stamp only: frame 48000 plays at host 7.0 s, the callback ran 20 ms before.
        let m = OutMark {
            first: 48_000,
            frames: 480,
            device_ns: 7 * S,
            callback_ns: 7 * S - 20_000_000,
            epoch: 0,
        };
        let p = pc.frame_at(7 * S - 5_000_000, Some(m));
        assert_eq!(p.mode, PlayMode::Stamp);
        assert!((p.out_frame - (48_000.0 - 240.0)).abs() < 1.0e-6, "{p:?}");

        // A snapshot that is not locked does not take over.
        pc.on_snapshot(snapshot(ClockQuality::Acquiring, 0, StampSource::Device));
        assert_eq!(pc.frame_at(7 * S, Some(m)).mode, PlayMode::Stamp);

        // Locked: the line. Host 7.0 s is 2.0 s after the base: frame 48000.
        pc.on_snapshot(snapshot(ClockQuality::Locked, 0, StampSource::Device));
        let p = pc.frame_at(7 * S + 1_000_000, Some(m));
        assert_eq!(p.mode, PlayMode::Line);
        assert!((p.out_frame - 48_048.0).abs() < 1.0e-6, "{p:?}");

        // No new block for more than 0.5 s: hold, at the last position.
        let shown = p.out_frame;
        let p = pc.frame_at(7 * S + 600_000_000, Some(m));
        assert_eq!((p.out_frame, p.mode), (shown, PlayMode::Hold));
        // Without a playhead the age of the snapshot decides.
        let p = pc.frame_at(7 * S + 100_000_000, None);
        assert_eq!(p.mode, PlayMode::Line);
        assert_eq!(pc.frame_at(8 * S, None).mode, PlayMode::Hold);
    }

    #[test]
    fn play_clock_never_goes_back_inside_an_epoch_and_jumps_on_a_new_one() {
        let mut pc = PlayClock::new(48_000.0);
        pc.on_snapshot(snapshot(ClockQuality::Locked, 0, StampSource::Device));
        let a = pc.frame_at(7 * S + 10_000_000, None);
        // An earlier instant (or a line that stepped back): the position waits.
        let b = pc.frame_at(7 * S, None);
        assert_eq!(b.out_frame, a.out_frame);
        let c = pc.frame_at(7 * S + 20_000_000, None);
        assert!(c.out_frame > a.out_frame);

        // Reopen: the frame counter restarts and the position follows at once.
        let m = OutMark {
            first: 0,
            frames: 480,
            device_ns: 9 * S,
            callback_ns: 9 * S - 20_000_000,
            epoch: 1,
        };
        let p = pc.frame_at(9 * S, Some(m));
        assert_eq!((p.mode, p.epoch), (PlayMode::Stamp, 1));
        assert!(p.out_frame.abs() < 1.0e-6, "{p:?}");
        // Before frame 0 of the new streams sounds the position is 0, not negative.
        let early = pc.frame_at(9 * S - 5_000_000, Some(m));
        assert_eq!((early.out_frame, early.mode), (0.0, PlayMode::Stamp));
        // A late snapshot of the old epoch is ignored once a newer one is in.
        pc.on_snapshot(snapshot(ClockQuality::Acquiring, 1, StampSource::Device));
        pc.on_snapshot(snapshot(ClockQuality::Locked, 0, StampSource::Device));
        assert_eq!(
            pc.frame_at(9 * S + 1_000_000, Some(m)).mode,
            PlayMode::Stamp
        );
    }

    #[test]
    fn stamp_mode_follows_the_source_of_the_map_and_never_passes_the_rendered_frames() {
        let m = OutMark {
            first: 48_000,
            frames: 480,
            device_ns: 7 * S,
            callback_ns: 7 * S - 20_000_000,
            epoch: 0,
        };
        // The map fell back to callback stamps: the stamp is read the same way.
        let mut pc = PlayClock::new(48_000.0);
        pc.on_snapshot(snapshot(ClockQuality::Acquiring, 0, StampSource::Callback));
        let p = pc.frame_at(7 * S - 15_000_000, Some(m));
        assert!((p.out_frame - 48_240.0).abs() < 1.0e-6, "{p:?}");
        // 30 ms after the request of a 10 ms block: capped at the end of the block.
        let p = pc.frame_at(7 * S + 10_000_000, Some(m));
        assert_eq!(p.out_frame, 48_480.0);
        // A stamp that is not a number of this world does not move the position.
        let mut pc = PlayClock::new(f64::NAN);
        assert_eq!(pc.frame_at(7 * S, Some(m)).mode, PlayMode::Hold);
    }
}
