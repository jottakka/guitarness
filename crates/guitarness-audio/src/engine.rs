//! The audio engine: one backend, the mixer on the output callback, the capture stage on
//! the input callback, and the analysis side that joins them.
//!
//! # Threads and queues
//!
//! ```text
//!  control thread --MixCmd--> [output callback: Mixer::render] --Stamp--------+
//!                                       |  `--MixEvent---------------------+  |
//!  [input callback: CaptureStage::process] --InEvent (Block, Onset)------+   |  |
//!                                       |  `--Frame (tuner)-----------+  |   |  |
//!  [error callback] --BackendNote (atomics + one ring push)--------+  |  |   |  |
//!                                                                  v  v  v   v  v
//!                                    analysis step (Manual pump or ~1 ms thread)
//!                                       |  owns the ClockMap, matches everything
//!                                       v
//!                                 EngineEvent ring --> client (EngineIo::events)
//! ```
//!
//! The callbacks never allocate, lock, print or block. A full ring drops the new item and
//! counts it ([`Stats::drops`]); the analysis side then sends [`EngineEvent::Dropped`].
//!
//! # The analysis step
//!
//! One function, `Analysis::step`, is used by the manual pump (tests) and by the analysis
//! thread (every [`EngineConfig::wake`], 1 ms). In order it:
//!
//! 1. drains the backend notes (xruns, device events);
//! 2. drains the output stamps into the [`ClockMap`] (`push_out`);
//! 3. drains the input ring: `Block` stamps go to `push_in`, onsets are converted to
//!    `InFrame` with the block mapping of the capture module and forwarded;
//! 4. forwards mixer events, and tuner frames (converted the same way);
//! 5. sends [`EngineEvent::Dropped`] for queues that dropped items;
//! 6. publishes the counters.
//!
//! A [`ClockSnapshot`] is sent every [`EngineConfig::clock_every_s`] of output time, on
//! every [`ClockStep`] and when the clock quality changes.
//!
//! # Xruns
//!
//! An xrun reported by the backend becomes a [`ClockMap::note_xrun`] at the first stamp
//! of that stream after the lost frames. Where that is comes from the backend: the error
//! callback receives the *boundary*, the frame counter of the stream at the instant the
//! backend saw the error ([`ErrorFn`]), and the notice belongs to the first stamp with
//! `first >= boundary`.
//!
//! - If that stamp has not reached the clock map yet, the notice waits for it and is
//!   applied just before it.
//! - If it has (backends deliver errors late: the cpal backend from a dispatcher thread,
//!   a few milliseconds after the fact, when the analysis has usually consumed the stamp),
//!   [`ClockMap::note_xrun_at`] takes the stream back to before that stamp and feeds it
//!   again with the notice in place.
//!
//! Either way the [`ClockStep`] is reported at the first stamp after the xrun, that stamp
//! does not enter the clock filter, and the "not judged" window is centred there. The
//! window covers the onsets delivered in between (see below).
//!
//! Limits:
//!
//! - **No boundary** (`None` from the backend). The notice is applied to the first stamp
//!   after the last block the engine had seen *when the error callback ran*. With a late
//!   delivery that is one block (or more) after the xrun. A loss above the jump threshold
//!   of the map (3 ms) still gets its step on the right stamp, because the jump rule holds
//!   such stamps back; a smaller one is reported one block late, after its first stamp
//!   went through the filter. The cpal backend always gives a boundary (exact, or the
//!   span of a lost one); `None` is for backends that cannot tell at all (the fake with
//!   `error_boundary` off).
//! - **CoreAudio** reports an overload (`kAudioDeviceProcessorOverload`) from a listener
//!   thread, not from the IO thread, so the order against the data callback is not
//!   defined: the boundary is the counter at that moment, the block where the overload
//!   showed or the next one. Approximate.
//! - **WASAPI** raises the input overrun on the stream thread right before the data
//!   callback of the affected packet: the boundary is exact. It does not report output
//!   underruns at all. The jump rule finds those from the stamps alone (3 stamps for a
//!   loss above 3 ms; a smaller one is absorbed by the loop).
//! - The map can go back over its last 64 stamps of a stream
//!   ([`UNDO_STAMPS`](crate::clock::UNDO_STAMPS)). A notice later than that is not moved
//!   to a later stamp ([`LateNotice::Unavailable`]): everything from the boundary to the
//!   last stamp pushed is "not judged", and the line is re-anchored at the next stamp.
//! - **Lost boundary** ([`XrunBoundary::Within`]: the backend's boundary queue was full).
//!   No place inside the span is guessed: everything from its start to the first stamp
//!   that cannot be before the xrun is "not judged", and the line is re-anchored there.
//! - Notices waiting for their stamp are kept up to 1024; one that has to make room
//!   leaves a window at its boundary instead of a step.
//!
//! # Playhead and host clock
//!
//! The output callback publishes the stamp of every block it renders in a [`Playhead`]
//! (atomics; [`AudioEngine::playhead`]). With [`AudioEngine::host_now_ns`] and the clock
//! snapshots, a [`PlayClock`](crate::playhead::PlayClock) tells the drawing code which
//! output frame is sounding. None of this feeds the judging.
//!
//! # "Not judged"
//!
//! Events within [`EngineConfig::glitch_before_s`] before to [`EngineConfig::glitch_after_s`]
//! after a clock step (100 ms, 250 ms) are not to be judged as Right or Wrong: the audio
//! glitched. [`OnsetNote::judged`] is `false` for an onset inside such a window, or while
//! the clock is `Degraded`/`Broken`. Onsets delivered *before* the step was detected cannot
//! be recalled, so every step is also followed by [`EngineEvent::Unjudged`] with the window
//! in input frames: the client must withdraw the judgement of earlier onsets inside it.
//!
//! # Device changes
//!
//! `Invalidated`, `DeviceLost` and `DeviceChanged` halt the callbacks at once (silence out,
//! input ignored; set by the error callback itself), mark the calibration stale and send
//! [`EngineEvent::Stopped`]. The streams themselves are dropped on the control thread, by
//! [`AudioEngine::pump`] or [`AudioEngine::poll`] (some backend streams are not `Send`).
//! [`AudioEngine::reopen`] opens again with a new epoch and fresh handles.
//!
//! # Origin offset
//!
//! The backend adds the input/output clock origin offset to the input stamps itself, so
//! the `ClockMap` runs with `origin_offset_ns = 0` and [`StreamInfo::origin_offset_ns`] is
//! only reported in [`Stats`].

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use guitarness_dsp::{Frame, OnsetEvent};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::backend::{Backend, ErrorFn, HostClock, InputFn, IoCallbacks, OutputFn, XrunBoundary};
use crate::capture::{CaptureOutputs, CaptureRings, CaptureStage, InEvent, RawOutput};
use crate::clock::{ClockConfig, ClockMap, ClockQuality, ClockSnapshot, ClockStep, LateNotice};
use crate::mixer::{MixCmd, MixEvent, Mixer};
use crate::playhead::{OutMark, Playhead};
use crate::sounds::SoundBank;
use crate::stats::{Ring, Shared, Stats, quality_code, store_max};
use crate::types::{AudioError, BackendEvent, ChannelPick, OpenRequest, Side, Stamp, StreamInfo};

const R: Ordering = Ordering::Relaxed;
const RINGS: [Ring; 8] = [
    Ring::MixCmds,
    Ring::Events,
    Ring::InEvents,
    Ring::Pitch,
    Ring::OutStamps,
    Ring::MixEvents,
    Ring::Backend,
    Ring::Tap,
];
/// Blocks of input mapping history kept for tuner frames.
const BLOCK_HISTORY: usize = 256;
/// Tuner frames waiting for the block that contains them.
const PITCH_PENDING: usize = 1024;
/// Backend xrun notes waiting for their stamp.
const NOTES: usize = 1024;
/// Glitch windows kept (oldest dropped first). Windows are not expired by time: a backlog
/// of any size in the input ring is still judged against every window it needs.
const WINDOWS: usize = 4096;
/// Critical events (steps, windows, stop, reopen) waiting for room in the event queue.
const CRITICAL: usize = 1024;

/// How the analysis step runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pump {
    /// The caller runs it with [`AudioEngine::pump`] (deterministic tests).
    Manual,
    /// A thread runs it every [`EngineConfig::wake`].
    Thread,
}

/// Engine settings. `Default` is the M2 plan.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Which devices to open.
    pub open: OpenRequest,
    /// Which input channel feeds the analysis.
    pub pick: ChannelPick,
    /// Frames per processing chunk of the capture stage (larger callbacks are chunked).
    pub max_block: usize,
    /// Clock tuning. `origin_offset_ns` is forced to 0 (the backend applies the offset).
    pub clock: ClockConfig,
    /// Capacity of the [`MixCmd`] queue.
    pub cmd_ring: usize,
    /// Capacity of the [`EngineEvent`] queue.
    pub event_ring: usize,
    /// Capacity of the [`MixEvent`] queue.
    pub mix_event_ring: usize,
    /// Capacity of the input block/onset queue.
    pub in_ring: usize,
    /// Capacity of the tuner frame queue.
    pub pitch_ring: usize,
    /// Capacity of the output stamp queue.
    pub out_stamp_ring: usize,
    /// Capacity of the backend event queue.
    pub backend_ring: usize,
    /// Send a [`ClockSnapshot`] every this many seconds of output time.
    pub clock_every_s: f64,
    /// Glitch window: seconds before a clock step that are not judged.
    pub glitch_before_s: f64,
    /// Glitch window: seconds after a clock step that are not judged.
    pub glitch_after_s: f64,
    /// Measure the duration of every callback with `Instant::now()` (two reads per callback;
    /// no allocation, no blocking system call). Feeds [`Stats::worst_in_ns`] and
    /// [`Stats::worst_out_ns`].
    pub time_callbacks: bool,
    /// Wake period of the analysis thread ([`Pump::Thread`]).
    pub wake: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            open: OpenRequest::default(),
            pick: ChannelPick::default(),
            max_block: 1024,
            clock: ClockConfig::default(),
            cmd_ring: 256,
            event_ring: 4096,
            mix_event_ring: 1024,
            in_ring: 4096,
            pitch_ring: 512,
            out_stamp_ring: 1024,
            backend_ring: 64,
            clock_every_s: 0.1,
            glitch_before_s: 0.1,
            glitch_after_s: 0.25,
            time_callbacks: true,
            wake: Duration::from_millis(1),
        }
    }
}

/// An onset (or its early notice) placed on the input frame counter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnsetNote {
    /// The detector event as it came (times in detector samples).
    pub event: OnsetEvent,
    /// Onset time as an input frame (`InFrame`, fractional). NaN if no block was known.
    pub t_in: f64,
    /// For `Heard`: the frame at which it was emitted. `None` for `Strum`.
    pub at_in: Option<f64>,
    /// `false`: not judged (audio glitch near a clock step, or the clock is unusable).
    pub judged: bool,
    /// Clock epoch.
    pub epoch: u32,
}

/// A tuner frame placed on the input frame counter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PitchNote {
    /// The frame.
    pub frame: Frame,
    /// Time of the frame as an input frame (fractional).
    pub t_in: f64,
    /// Clock epoch.
    pub epoch: u32,
}

/// What the engine tells the client.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineEvent {
    /// Onset or early notice, with its input frame and the "judged" flag.
    Onset(OnsetNote),
    /// Tuner frame.
    Pitch(PitchNote),
    /// Mixer event (output frames).
    Mix(MixEvent),
    /// Clock map copy (periodic, on steps, on quality change).
    Clock(ClockSnapshot),
    /// A clock step (xrun or discontinuity) on one stream.
    Step(ClockStep),
    /// Follows every [`EngineEvent::Step`]: onsets with `from_in <= t_in <= to_in` (input
    /// frames) are not judged, including those already delivered.
    Unjudged {
        /// Clock epoch.
        epoch: u32,
        /// First input frame of the window.
        from_in: f64,
        /// Last input frame of the window.
        to_in: f64,
    },
    /// The backend reported this.
    Backend(BackendEvent),
    /// Device event: the callbacks are halted and the streams are being closed. The
    /// calibration is stale.
    Stopped(BackendEvent),
    /// [`AudioEngine::reopen`] succeeded; the clock map starts epoch `epoch`.
    Reopened {
        /// New epoch.
        epoch: u32,
    },
    /// `n` items of `ring` were dropped since the last notice.
    Dropped {
        /// Which queue.
        ring: Ring,
        /// How many.
        n: u32,
    },
}

/// Sends commands to the mixer. Never blocks.
pub struct MixerHandle {
    tx: Producer<MixCmd>,
    sh: Arc<Shared>,
}

impl MixerHandle {
    /// Queues `cmd`. `false` (and a counted drop) if the queue is full or the engine is gone.
    pub fn send(&mut self, cmd: MixCmd) -> bool {
        // A push to a ring whose consumer is gone succeeds silently: refuse it.
        if !self.tx.is_abandoned() && self.tx.push(cmd).is_ok() {
            true
        } else {
            self.sh.drop_cmds.fetch_add(1, R);
            false
        }
    }

    /// Free slots in the command queue.
    pub fn slots(&self) -> usize {
        self.tx.slots()
    }
}

/// Switches the raw input tap on and off and reads it. The tap is a mono copy of the
/// analysis channel at the input rate, in detector time (see [`TapHandle::origin`]).
///
/// The handle also gives access to the raw ring, a second copy of the same samples in
/// whole blocks addressed by input frame, for the tape ([`TapHandle::take_raw`]). The two
/// are independent: the tap belongs to the calibrator.
pub struct TapHandle {
    out: CaptureOutputs,
}

impl TapHandle {
    /// Starts or stops the tap. Takes effect at the next input callback.
    pub fn set(&self, on: bool) {
        self.out.set_tap(on);
    }

    /// Detector time of the first tap sample since the tap was last switched on.
    pub fn origin(&self) -> u64 {
        self.out.tap_origin()
    }

    /// Samples waiting.
    pub fn available(&self) -> usize {
        self.out.tap.slots()
    }

    /// Copies up to `dst.len()` samples out; returns how many.
    pub fn read(&mut self, dst: &mut [f32]) -> usize {
        let n = self.out.tap.slots().min(dst.len());
        if n == 0 {
            return 0;
        }
        let Ok(chunk) = self.out.tap.read_chunk(n) else {
            return 0;
        };
        let (a, b) = chunk.as_slices();
        dst[..a.len()].copy_from_slice(a);
        dst[a.len()..n].copy_from_slice(b);
        chunk.commit_all();
        n
    }

    /// Starts or stops the raw ring (off by default). Takes effect at the next input
    /// callback. While on, every chunk the capture stage processes goes to the ring whole,
    /// with its [`RawMark`](crate::capture::RawMark); a chunk that does not fit is dropped
    /// whole and counted ([`RawOutput::dropped_blocks`]).
    pub fn set_raw(&self, on: bool) {
        self.out.set_raw(on);
    }

    /// Hands out the reader of the raw ring: once per handle, `None` afterwards. It is
    /// `Send` (one consumer, the tape thread). A reopen gives a new handle with a new
    /// ring: the reader of the old one only has what the old streams left in it.
    pub fn take_raw(&mut self) -> Option<RawOutput> {
        self.out.take_raw()
    }
}

/// What the client gets from [`AudioEngine::start`].
pub struct EngineIo {
    /// Commands to the mixer.
    pub mix: MixerHandle,
    /// Events from the analysis side.
    pub events: Consumer<EngineEvent>,
    /// The raw input tap.
    pub tap: TapHandle,
}

// ------------------------------------------------------------------ callbacks

/// Runs `f` under `assert_no_alloc` when the `rt-audit` feature is on.
#[inline(always)]
fn rt<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(feature = "rt-audit")]
    {
        assert_no_alloc::assert_no_alloc(f)
    }
    #[cfg(not(feature = "rt-audit"))]
    {
        f()
    }
}

struct OutStage {
    mixer: Mixer,
    channels: u16,
    base_dropped: u64,
    /// Engine epoch of the streams this stage runs on.
    epoch: u32,
}

struct InStage {
    stage: CaptureStage,
    base_nonfinite: u64,
    base_dropped_in: u64,
    base_dropped_pitch: u64,
    base_tap: u64,
}

/// An error-callback item. `at`: the boundary the backend gave for an xrun (the frame
/// counter of that stream when it saw the error, or the span it lies in). `in_end`/
/// `out_end`: end frame of the last block of each stream when the error callback ran, the
/// fallback when there is no boundary.
#[derive(Clone, Copy)]
struct Note {
    ev: BackendEvent,
    at: Option<XrunBoundary>,
    in_end: u64,
    out_end: u64,
}

fn side_idx(side: Side) -> usize {
    usize::from(side == Side::Out)
}

fn fatal(ev: BackendEvent) -> bool {
    matches!(
        ev,
        BackendEvent::Invalidated | BackendEvent::DeviceLost | BackendEvent::DeviceChanged
    )
}

fn fatal_code(ev: BackendEvent) -> u8 {
    match ev {
        BackendEvent::Invalidated => 1,
        BackendEvent::DeviceLost => 2,
        BackendEvent::DeviceChanged => 3,
        _ => 0,
    }
}

fn fatal_of(code: u8) -> Option<BackendEvent> {
    match code {
        1 => Some(BackendEvent::Invalidated),
        2 => Some(BackendEvent::DeviceLost),
        3 => Some(BackendEvent::DeviceChanged),
        _ => None,
    }
}

fn output_cb(
    sh: Arc<Shared>,
    mut rx: Consumer<Box<OutStage>>,
    mut stamps: Producer<Stamp>,
    playhead: Playhead,
    timing: bool,
) -> OutputFn {
    let mut stage: Option<Box<OutStage>> = None;
    Box::new(move |out: &mut [f32], stamp: Stamp| {
        rt(|| {
            let t0 = timing.then(Instant::now);
            'run: {
                if stage.is_none()
                    && let Ok(s) = rx.pop()
                {
                    stage = Some(s);
                }
                let st = match stage.as_mut() {
                    Some(s) if !sh.halted.load(Ordering::Acquire) => s,
                    _ => {
                        out.fill(0.0);
                        sh.skipped_out.fetch_add(1, R);
                        break 'run;
                    }
                };
                st.mixer.render(out, st.channels, stamp.first);
                let mut bad = 0u64;
                for s in out.iter_mut() {
                    if !s.is_finite() {
                        *s = 0.0;
                        bad += 1;
                    }
                }
                if bad > 0 {
                    sh.nonfinite_out.fetch_add(bad, R);
                }
                sh.out_end.store(stamp.first + u64::from(stamp.frames), R);
                playhead.publish(OutMark {
                    first: stamp.first,
                    frames: stamp.frames,
                    device_ns: stamp.device_ns,
                    callback_ns: stamp.callback_ns,
                    epoch: st.epoch,
                });
                if stamps.push(stamp).is_err() {
                    sh.drop_out_stamps.fetch_add(1, R);
                }
                sh.drop_mix_events
                    .store(st.base_dropped + st.mixer.dropped_events(), R);
                sh.cb_out.fetch_add(1, R);
                sh.frames_out.fetch_add(u64::from(stamp.frames), R);
            }
            if let Some(t0) = t0 {
                store_max(&sh.worst_out_ns, t0.elapsed().as_nanos() as u64);
            }
        });
    })
}

/// State of the input callback. When it is dropped (the streams closed) the capture stage
/// goes back to the control side through `back`, so that it can be flushed.
struct InCb {
    sh: Arc<Shared>,
    rx: Consumer<Box<InStage>>,
    back: Producer<Box<InStage>>,
    stage: Option<Box<InStage>>,
    timing: bool,
}

impl InCb {
    fn run(&mut self, input: &[f32], stamp: Stamp) {
        rt(|| {
            let sh = &*self.sh;
            let t0 = self.timing.then(Instant::now);
            'run: {
                if self.stage.is_none()
                    && let Ok(s) = self.rx.pop()
                {
                    self.stage = Some(s);
                }
                let st = match self.stage.as_mut() {
                    Some(s) if !sh.halted.load(Ordering::Acquire) => s,
                    _ => {
                        sh.skipped_in.fetch_add(1, R);
                        break 'run;
                    }
                };
                st.stage.process(input, stamp);
                sh.in_end.store(stamp.first + u64::from(stamp.frames), R);
                let cs = st.stage.stats();
                sh.nonfinite_in.store(st.base_nonfinite + cs.nonfinite, R);
                sh.drop_in_events
                    .store(st.base_dropped_in + cs.dropped_in, R);
                sh.drop_pitch
                    .store(st.base_dropped_pitch + cs.dropped_pitch, R);
                sh.drop_tap.store(st.base_tap + cs.tap_dropped, R);
                sh.cb_in.fetch_add(1, R);
                sh.frames_in.fetch_add(u64::from(stamp.frames), R);
            }
            if let Some(t0) = t0 {
                store_max(&sh.worst_in_ns, t0.elapsed().as_nanos() as u64);
            }
        });
    }
}

impl Drop for InCb {
    fn drop(&mut self) {
        let s = self.stage.take().or_else(|| self.rx.pop().ok());
        if let Some(s) = s {
            let _ = self.back.push(s);
        }
    }
}

fn input_cb(
    sh: Arc<Shared>,
    rx: Consumer<Box<InStage>>,
    back: Producer<Box<InStage>>,
    timing: bool,
) -> InputFn {
    let mut cb = InCb {
        sh,
        rx,
        back,
        stage: None,
        timing,
    };
    Box::new(move |input: &[f32], stamp: Stamp| cb.run(input, stamp))
}

fn error_cb(sh: Arc<Shared>, mut tx: Producer<Note>) -> ErrorFn {
    Box::new(move |ev: BackendEvent, at: Option<XrunBoundary>| {
        rt(|| {
            sh.backend_events.fetch_add(1, R);
            match ev {
                BackendEvent::XrunIn => {
                    sh.xruns_in.fetch_add(1, R);
                }
                BackendEvent::XrunOut => {
                    sh.xruns_out.fetch_add(1, R);
                }
                _ => {}
            }
            if fatal(ev) {
                // Latched: survives a full note queue.
                sh.fatal.store(fatal_code(ev), Ordering::Release);
                sh.calib_stale.store(true, Ordering::Release);
                sh.lost.store(true, Ordering::Release);
                sh.halted.store(true, Ordering::Release);
            }
            let note = Note {
                ev,
                at,
                in_end: sh.in_end.load(R),
                out_end: sh.out_end.load(R),
            };
            if tx.push(note).is_err() {
                sh.drop_backend.fetch_add(1, R);
            }
        });
    })
}

// ------------------------------------------------------------------ opening

/// Consumer sides of every queue that ends in the analysis step.
struct Rings {
    out_stamps: Consumer<Stamp>,
    in_events: Consumer<InEvent>,
    pitch: Consumer<Frame>,
    mix_events: Consumer<MixEvent>,
    notes: Consumer<Note>,
    /// The capture stage, returned by the input callback when it is dropped.
    stage_back: Consumer<Box<InStage>>,
}

struct Opened<S> {
    streams: S,
    info: StreamInfo,
    rings: Rings,
    mix: MixerHandle,
    tap: TapHandle,
    bank: SoundBank,
}

fn open_pipe<B: Backend>(
    backend: &B,
    cfg: &EngineConfig,
    bank: &SoundBank,
    sh: &Arc<Shared>,
    playhead: &Playhead,
    epoch: u32,
) -> Result<Opened<B::Streams>, AudioError> {
    let (cmd_tx, cmd_rx) = RingBuffer::<MixCmd>::new(cfg.cmd_ring.max(1));
    let (mixev_tx, mixev_rx) = RingBuffer::<MixEvent>::new(cfg.mix_event_ring.max(1));
    let (stamp_tx, stamp_rx) = RingBuffer::<Stamp>::new(cfg.out_stamp_ring.max(1));
    let (note_tx, note_rx) = RingBuffer::<Note>::new(cfg.backend_ring.max(1));
    let (mut out_h_tx, out_h_rx) = RingBuffer::<Box<OutStage>>::new(1);
    let (mut in_h_tx, in_h_rx) = RingBuffer::<Box<InStage>>::new(1);
    let (back_tx, back_rx) = RingBuffer::<Box<InStage>>::new(1);

    sh.in_end.store(0, R);
    sh.out_end.store(0, R);
    sh.fatal.store(0, Ordering::Release);
    sh.lost.store(false, Ordering::Release);
    sh.halted.store(false, Ordering::Release);

    let io = IoCallbacks {
        input: input_cb(sh.clone(), in_h_rx, back_tx, cfg.time_callbacks),
        output: output_cb(
            sh.clone(),
            out_h_rx,
            stamp_tx,
            playhead.clone(),
            cfg.time_callbacks,
        ),
        error: error_cb(sh.clone(), note_tx),
    };
    // The stages need the real rates, known only after opening: the callbacks skip
    // (silence out, input ignored) until the control thread hands them over.
    let (streams, info) = backend.open(&cfg.open, io)?;

    let (fs_in, fs_out) = (f64::from(info.fs_in), f64::from(info.fs_out));
    let (caprings, mut outs) = CaptureRings::with_capacity(fs_in, cfg.in_ring, cfg.pitch_ring);
    let events = std::mem::replace(&mut outs.events, RingBuffer::new(1).1);
    let pitch = std::mem::replace(&mut outs.pitch, RingBuffer::new(1).1);
    let stage = CaptureStage::new(fs_in, info.ch_in, cfg.pick, cfg.max_block, caprings);
    let bank = if (bank.sample_rate() - fs_out).abs() < 0.5 {
        bank.clone()
    } else {
        SoundBank::new(fs_out)
    };
    let mixer = Mixer::new(bank.clone(), cmd_rx, mixev_tx);

    let ins = InStage {
        stage,
        base_nonfinite: sh.nonfinite_in.load(R),
        base_dropped_in: sh.drop_in_events.load(R),
        base_dropped_pitch: sh.drop_pitch.load(R),
        base_tap: sh.drop_tap.load(R),
    };
    let outs_stage = OutStage {
        mixer,
        channels: info.ch_out,
        base_dropped: sh.drop_mix_events.load(R),
        epoch,
    };
    // Cannot fail: each hand-over queue is empty and has one slot.
    let _ = in_h_tx.push(Box::new(ins));
    let _ = out_h_tx.push(Box::new(outs_stage));

    sh.origin_offset_ns.store(info.origin_offset_ns, R);
    Ok(Opened {
        streams,
        info,
        rings: Rings {
            out_stamps: stamp_rx,
            in_events: events,
            pitch,
            mix_events: mixev_rx,
            notes: note_rx,
            stage_back: back_rx,
        },
        mix: MixerHandle {
            tx: cmd_tx,
            sh: sh.clone(),
        },
        tap: TapHandle { out: outs },
        bank,
    })
}

// ------------------------------------------------------------------ analysis

/// What survives a reopen.
struct Link {
    events: Producer<EngineEvent>,
    reported: [u64; 8],
}

#[derive(Clone, Copy)]
struct BlockMap {
    first: u64,
    det_first: u64,
    frames: u32,
}

/// The analysis side: owns the clock map and every consumer. Step it with
/// `Analysis::step`.
struct Analysis {
    sh: Arc<Shared>,
    link: Link,
    rings: Rings,
    clock: ClockMap,
    epoch: u32,
    fs_in: f64,
    before_in: f64,
    after_in: f64,
    clock_every_out: u64,
    next_clock_out: u64,
    last_quality: ClockQuality,
    /// Latest input block.
    cur: Option<BlockMap>,
    /// An input stamp has been processed in this epoch.
    in_seen: bool,
    history: VecDeque<BlockMap>,
    pitch_wait: VecDeque<Frame>,
    /// Detector time of the first block after the latest lost block event: onsets that
    /// start before it cannot be placed on the input line.
    gap_det: u64,
    /// Xrun notices waiting for the first stamp of their stream at or after the boundary.
    notes: VecDeque<(Side, u64)>,
    /// `first` and end frame of the last stamp of each stream (input, output) pushed to the
    /// clock map.
    pushed: [Option<(u64, u64)>; 2],
    /// Scratch for the steps of a late notice.
    late_steps: Vec<ClockStep>,
    windows: VecDeque<(f64, f64)>,
    /// Output-side spans whose window needs the input mapping: output frames, inclusive.
    deferred: Vec<(u64, u64)>,
    /// Events that must not be lost, in order, waiting for room.
    crit: VecDeque<EngineEvent>,
    stopped_sent: bool,
}

impl Analysis {
    fn new(
        sh: Arc<Shared>,
        cfg: &EngineConfig,
        link: Link,
        rings: Rings,
        info: &StreamInfo,
        epoch: u32,
    ) -> Self {
        let (fs_in, fs_out) = (f64::from(info.fs_in), f64::from(info.fs_out));
        let mut ccfg = cfg.clock;
        ccfg.origin_offset_ns = 0;
        let mut clock = ClockMap::new(fs_in, fs_out, ccfg);
        for _ in 0..epoch {
            clock.reset();
        }
        let mut a = Self {
            sh,
            link,
            rings,
            last_quality: clock.quality(),
            clock,
            epoch,
            fs_in,
            before_in: cfg.glitch_before_s * fs_in,
            after_in: cfg.glitch_after_s * fs_in,
            clock_every_out: ((cfg.clock_every_s * fs_out).round() as u64).max(1),
            next_clock_out: 0,
            cur: None,
            in_seen: false,
            history: VecDeque::with_capacity(BLOCK_HISTORY + 1),
            pitch_wait: VecDeque::with_capacity(PITCH_PENDING + 1),
            gap_det: 0,
            notes: VecDeque::new(),
            pushed: [None; 2],
            late_steps: Vec::with_capacity(crate::clock::UNDO_STAMPS),
            windows: VecDeque::new(),
            deferred: Vec::new(),
            crit: VecDeque::new(),
            stopped_sent: false,
        };
        if epoch > 0 {
            a.emit_critical(EngineEvent::Reopened { epoch });
        }
        a
    }

    /// Sends what is waiting, in order, as far as the queue has room.
    fn flush_critical(&mut self) {
        while let Some(e) = self.crit.front().copied() {
            if self.link.events.push(e).is_err() {
                break;
            }
            self.crit.pop_front();
        }
    }

    /// Ordinary event: dropped (and counted) if the queue is full, or if a critical event
    /// is still waiting (order is kept).
    fn emit(&mut self, e: EngineEvent) {
        self.flush_critical();
        if !self.crit.is_empty() || self.link.events.push(e).is_err() {
            self.sh.drop_events.fetch_add(1, R);
        }
    }

    /// Event that is retried until it fits.
    fn emit_critical(&mut self, e: EngineEvent) {
        self.flush_critical();
        if self.crit.is_empty() && self.link.events.push(e).is_ok() {
            return;
        }
        if self.crit.len() < CRITICAL {
            self.crit.push_back(e);
        } else {
            self.sh.drop_events.fetch_add(1, R);
        }
    }

    fn emit_clock(&mut self) {
        let snap = self.clock.snapshot();
        self.last_quality = snap.quality();
        self.emit(EngineEvent::Clock(snap));
    }

    /// One pass over every queue. Cheap when idle.
    ///
    /// The stamp and block queues are drained only up to what they held when the pass
    /// began. Backend notes are read after that count, so a note that was pushed before a
    /// stamp (the error callback runs before the block that follows the glitch) is always
    /// seen before that stamp is processed.
    fn step(&mut self) {
        self.flush_critical();
        let n_out = self.rings.out_stamps.slots();
        let n_in = self.rings.in_events.slots();
        self.drain_notes();
        self.drain_out(n_out);
        self.drain_in(n_in);
        self.resolve_deferred();
        self.drain_mix();
        self.drain_pitch();
        if self.clock.quality() != self.last_quality {
            self.emit_clock();
        }
        self.report_drops();
        self.sh.quality.store(quality_code(self.clock.quality()), R);
    }

    /// After the callbacks have stopped: the last pass.
    fn finish(&mut self) {
        self.step();
    }

    fn drain_notes(&mut self) {
        while let Ok(n) = self.rings.notes.pop() {
            self.emit(EngineEvent::Backend(n.ev));
            match n.ev {
                BackendEvent::XrunIn => self.on_xrun(Side::In, n.at, n.in_end),
                BackendEvent::XrunOut => self.on_xrun(Side::Out, n.at, n.out_end),
                ev if fatal(ev) => self.mark_stopped(ev),
                _ => {}
            }
        }
        // The note queue may have been full when the fatal event came.
        if let Some(ev) = fatal_of(self.sh.fatal.load(Ordering::Acquire)) {
            self.mark_stopped(ev);
        }
    }

    fn mark_stopped(&mut self, ev: BackendEvent) {
        if !self.stopped_sent {
            self.stopped_sent = true;
            self.emit_critical(EngineEvent::Stopped(ev));
        }
    }

    /// An xrun notice of the backend. `boundary`: where the backend says the stream was;
    /// `seen_end`: where the engine had seen it when the error callback ran.
    fn on_xrun(&mut self, side: Side, boundary: Option<XrunBoundary>, seen_end: u64) {
        let pushed = self.pushed[side_idx(side)];
        let pushed_end = pushed.map(|(_, end)| end);
        let at = match boundary {
            // The backend cannot tell: the first stamp after what had been seen.
            None => return self.hold_note(side, seen_end),
            // The backend lost the exact place. No guess: the notice re-anchors the line
            // at the first stamp that cannot be before the xrun, and nothing from the
            // earliest place to there is judged.
            Some(XrunBoundary::Within { from, to }) => {
                self.hold_note(side, to);
                return self.add_span(side, from, pushed_end.map_or(to, |end| end.max(to)));
            }
            Some(XrunBoundary::Exact(at)) => at,
        };
        if pushed.is_none_or(|(first, _)| first < at) {
            // In time: the stamp at the boundary is still to come.
            return self.hold_note(side, at);
        }
        // Late: that stamp is in the map already. The map goes back and redoes it.
        let mut steps = std::mem::take(&mut self.late_steps);
        let done = self.clock.note_xrun_at(side, at, |s| steps.push(s));
        for s in steps.drain(..) {
            self.on_step(s);
        }
        self.late_steps = steps;
        if done == LateNotice::Unavailable {
            // Too late for the history of the map: the stamps after the xrun went into the
            // loop. Not judged from the boundary to where the notice re-anchors.
            self.add_span(side, at, pushed_end.unwrap_or(at));
        }
    }

    /// The xrun is applied just before the first stamp of `side` with `first >= at`.
    fn hold_note(&mut self, side: Side, at: u64) {
        if self.notes.len() >= NOTES {
            // Never silently: a notice that cannot wait any more leaves its window.
            if let Some((s, a)) = self.notes.pop_front() {
                self.add_span(s, a, a);
            }
        }
        self.notes.push_back((side, at));
    }

    fn apply_notes(&mut self, side: Side, first: u64) {
        let mut i = 0;
        while i < self.notes.len() {
            let (s, at) = self.notes[i];
            if s == side && first >= at {
                self.notes.remove(i);
                self.clock.note_xrun(side);
            } else {
                i += 1;
            }
        }
    }

    fn drain_out(&mut self, n: usize) {
        for _ in 0..n {
            let Ok(s) = self.rings.out_stamps.pop() else {
                break;
            };
            self.apply_notes(Side::Out, s.first);
            if let Some(step) = self.clock.push_out(s) {
                self.on_step(step);
            }
            let end = s.first + u64::from(s.frames);
            self.pushed[side_idx(Side::Out)] = Some((s.first, end));
            if end >= self.next_clock_out {
                self.next_clock_out = end + self.clock_every_out;
                self.emit_clock();
            }
        }
    }

    fn drain_in(&mut self, n: usize) {
        for _ in 0..n {
            let Ok(ev) = self.rings.in_events.pop() else {
                break;
            };
            match ev {
                InEvent::Block { stamp, det_first } => {
                    // Detector time runs on frame by frame: a block that does not start where
                    // the previous one ended means a block event was dropped in between.
                    let expect = self.cur.map_or(0, |b| b.det_first + u64::from(b.frames));
                    if det_first != expect {
                        self.gap_det = det_first;
                    }
                    self.in_seen = true;
                    self.apply_notes(Side::In, stamp.first);
                    if let Some(step) = self.clock.push_in(stamp) {
                        self.on_step(step);
                    }
                    let end = stamp.first + u64::from(stamp.frames);
                    self.pushed[side_idx(Side::In)] = Some((stamp.first, end));
                    let b = BlockMap {
                        first: stamp.first,
                        det_first,
                        frames: stamp.frames,
                    };
                    self.cur = Some(b);
                    if self.history.len() >= BLOCK_HISTORY {
                        self.history.pop_front();
                    }
                    self.history.push_back(b);
                }
                InEvent::Onset(e) => {
                    // The capture stage publishes an onset only behind its own block, so
                    // `cur` is the block it belongs to and it is placed at once.
                    let cur = self.cur;
                    self.on_onset(e, cur);
                }
            }
        }
    }

    fn drain_mix(&mut self) {
        while let Ok(e) = self.rings.mix_events.pop() {
            self.emit(EngineEvent::Mix(e));
        }
    }

    fn drain_pitch(&mut self) {
        while let Ok(f) = self.rings.pitch.pop() {
            if self.pitch_wait.len() >= PITCH_PENDING {
                self.pitch_wait.pop_front();
                self.sh.drop_pitch_an.fetch_add(1, R);
            }
            self.pitch_wait.push_back(f);
        }
        while let Some(f) = self.pitch_wait.front().copied() {
            let Some(cur) = self.cur else {
                break;
            };
            let det = f.t * self.fs_in;
            // Frames come from the block that is not known yet: wait for its stamp.
            if det > (cur.det_first + u64::from(cur.frames)) as f64 + 1.0 {
                break;
            }
            self.pitch_wait.pop_front();
            // The frame must lie inside a block we know; otherwise that block's event was
            // dropped and the time cannot be placed.
            let found = self.history.iter().rev().find(|b| {
                det >= b.det_first as f64 - 1.0e-6
                    && det <= (b.det_first + u64::from(b.frames)) as f64 + 1.0
            });
            let Some(b) = found.copied() else {
                self.sh.drop_pitch_an.fetch_add(1, R);
                continue;
            };
            let t_in = b.first as f64 + (det - b.det_first as f64);
            self.emit(EngineEvent::Pitch(PitchNote {
                frame: f,
                t_in,
                epoch: self.epoch,
            }));
        }
    }

    fn in_window(&self, x: f64) -> bool {
        self.windows.iter().any(|&(lo, hi)| x >= lo && x <= hi)
    }

    fn on_onset(&mut self, e: OnsetEvent, block: Option<BlockMap>) {
        // Safety net: an onset that starts after its block ended or before the last input
        // gap sits behind a stale or lost block and cannot be placed.
        let (t0, t1) = match e {
            OnsetEvent::Heard { t, at } => (t, at.max(t)),
            OnsetEvent::Strum(s) => (s.t, s.t),
        };
        let block = block.filter(|b| {
            t1 <= (b.det_first + u64::from(b.frames)) as f64 + 1.0 && t0 >= self.gap_det as f64
        });
        let (t_in, at_in) = match (block, e) {
            (Some(b), OnsetEvent::Heard { t, at }) => {
                let m = |x: f64| b.first as f64 + (x - b.det_first as f64);
                (m(t), Some(m(at)))
            }
            (Some(b), OnsetEvent::Strum(s)) => (b.first as f64 + (s.t - b.det_first as f64), None),
            (None, OnsetEvent::Heard { .. }) => (f64::NAN, Some(f64::NAN)),
            (None, OnsetEvent::Strum(_)) => (f64::NAN, None),
        };
        // Only a locked clock is good enough to judge rhythm.
        let judged = self.clock.quality() == ClockQuality::Locked
            && t_in.is_finite()
            && !self.in_window(t_in);
        self.sh.onsets.fetch_add(1, R);
        if !judged {
            self.sh.onsets_unjudged.fetch_add(1, R);
        }
        self.emit(EngineEvent::Onset(OnsetNote {
            event: e,
            t_in,
            at_in,
            judged,
            epoch: self.epoch,
        }));
    }

    /// A "not judged" window around the input frames `lo..=hi`.
    fn add_window(&mut self, lo: f64, hi: f64) {
        let (from_in, to_in) = (lo - self.before_in, hi + self.after_in);
        if self.windows.len() >= WINDOWS {
            self.windows.pop_front();
        }
        self.windows.push_back((from_in, to_in));
        self.emit_critical(EngineEvent::Unjudged {
            epoch: self.epoch,
            from_in,
            to_in,
        });
    }

    fn on_step(&mut self, step: ClockStep) {
        match step.side {
            Side::In => self.sh.steps_in.fetch_add(1, R),
            Side::Out => self.sh.steps_out.fetch_add(1, R),
        };
        self.emit_critical(EngineEvent::Step(step));
        self.add_span(step.side, step.frame, step.frame);
        self.emit_clock();
    }

    /// A "not judged" window around the frames `lo..=hi` of the stream of `side`.
    fn add_span(&mut self, side: Side, lo: u64, hi: u64) {
        match side {
            Side::In => self.add_window(lo as f64, hi as f64),
            // Needs the input line: placed after this pass has read the input stamps.
            Side::Out => self.deferred.push((lo, hi)),
        }
    }

    /// Windows of output-side spans, once the input line is known and current.
    fn resolve_deferred(&mut self) {
        if !self.in_seen || self.deferred.is_empty() {
            return;
        }
        let snap = self.clock.snapshot();
        for (lo, hi) in std::mem::take(&mut self.deferred) {
            self.add_window(snap.in_of_out(lo as f64), snap.in_of_out(hi as f64));
        }
    }

    /// Sends `Dropped` for queues that lost items since the last notice. A notice that
    /// does not fit is retried at the next step.
    fn report_drops(&mut self) {
        for (i, ring) in RINGS.into_iter().enumerate() {
            let total = self.sh.ring_drops(ring);
            let n = total.wrapping_sub(self.link.reported[i]);
            if n == 0 {
                continue;
            }
            let n32 = n.min(u64::from(u32::MAX));
            let ev = EngineEvent::Dropped {
                ring,
                n: n32 as u32,
            };
            if self.link.events.push(ev).is_ok() {
                self.link.reported[i] = self.link.reported[i].wrapping_add(n32);
            }
        }
    }
}

// ------------------------------------------------------------------ engine

#[cfg(test)]
static FAIL_SPAWN: AtomicBool = AtomicBool::new(false);

type Worker = (mpsc::Sender<Analysis>, JoinHandle<Option<Analysis>>);

/// Starts the analysis thread; it waits for the analysis state to be sent. Spawning first
/// and sending later lets `reopen` fail before anything is committed.
fn spawn_worker(stop: Arc<AtomicBool>, wake: Duration) -> Result<Worker, AudioError> {
    #[cfg(test)]
    if FAIL_SPAWN.load(Ordering::Relaxed) {
        return Err(AudioError::Backend("analysis thread: injected".to_string()));
    }
    let (tx, rx) = mpsc::channel::<Analysis>();
    let th = thread::Builder::new()
        .name("guitarness-analysis".into())
        .spawn(move || {
            let mut a = rx.recv().ok()?;
            while !stop.load(Ordering::Acquire) {
                a.step();
                thread::park_timeout(wake);
            }
            a.step();
            Some(a)
        })
        .map_err(|e| AudioError::Backend(format!("analysis thread: {e}")))?;
    Ok((tx, th))
}

/// The running engine. Owns the streams; dropping it stops everything.
pub struct AudioEngine<B: Backend> {
    backend: B,
    cfg: EngineConfig,
    mode: Pump,
    bank: SoundBank,
    streams: Option<B::Streams>,
    info: StreamInfo,
    sh: Arc<Shared>,
    playhead: Playhead,
    epoch: u32,
    analysis: Option<Analysis>,
    link: Option<Link>,
    thread: Option<JoinHandle<Option<Analysis>>>,
    stop: Arc<AtomicBool>,
}

impl<B: Backend> AudioEngine<B> {
    /// Opens the streams and starts the engine. `bank` is regenerated at the output rate
    /// if it was built at another one.
    pub fn start(
        backend: B,
        mut cfg: EngineConfig,
        bank: SoundBank,
        pump: Pump,
    ) -> Result<(Self, EngineIo), AudioError> {
        cfg.clock.origin_offset_ns = 0;
        let sh = Arc::new(Shared::default());
        let (events_tx, events_rx) = RingBuffer::<EngineEvent>::new(cfg.event_ring.max(2));
        let link = Link {
            events: events_tx,
            reported: [0; 8],
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker = match pump {
            Pump::Manual => None,
            Pump::Thread => Some(spawn_worker(stop.clone(), cfg.wake)?),
        };
        let playhead = Playhead::new();
        let opened = open_pipe(&backend, &cfg, &bank, &sh, &playhead, 0)?;
        let analysis = Analysis::new(sh.clone(), &cfg, link, opened.rings, &opened.info, 0);
        let mut eng = Self {
            backend,
            cfg,
            mode: pump,
            bank: opened.bank,
            streams: Some(opened.streams),
            info: opened.info,
            sh,
            playhead,
            epoch: 0,
            analysis: None,
            link: None,
            thread: None,
            stop,
        };
        eng.install(analysis, worker);
        Ok((
            eng,
            EngineIo {
                mix: opened.mix,
                events: events_rx,
                tap: opened.tap,
            },
        ))
    }

    fn install(&mut self, a: Analysis, worker: Option<Worker>) {
        match worker {
            None => self.analysis = Some(a),
            Some((tx, th)) => {
                // Cannot fail: the worker is waiting on the other end.
                let _ = tx.send(a);
                self.thread = Some(th);
            }
        }
    }

    /// Stops the analysis thread (if any) and takes the analysis state back.
    fn halt_analysis(&mut self) {
        if let Some(t) = self.thread.take() {
            self.stop.store(true, Ordering::Release);
            t.thread().unpark();
            if let Ok(Some(a)) = t.join() {
                self.analysis = Some(a);
            }
        }
    }

    /// With the callbacks stopped: gets the capture stage back, flushes its open onset
    /// group, and runs the last pass.
    fn drain_final(a: &mut Analysis) {
        if let Ok(mut s) = a.rings.stage_back.pop() {
            s.stage.flush();
        }
        a.finish();
    }

    /// Runs one analysis step (only in [`Pump::Manual`]; in thread mode this is
    /// [`AudioEngine::poll`]). Also closes the streams after a device event.
    pub fn pump(&mut self) {
        if let Some(a) = self.analysis.as_mut() {
            a.step();
        }
        let was_open = self.streams.is_some();
        self.poll();
        if was_open
            && self.streams.is_none()
            && let Some(a) = self.analysis.as_mut()
        {
            a.step();
        }
    }

    /// Control-thread upkeep: after a device event, drops the streams (outside any
    /// callback). Returns whether the streams are still open.
    pub fn poll(&mut self) -> bool {
        if self.sh.lost.load(Ordering::Acquire) {
            drop(self.streams.take());
        }
        self.streams.is_some()
    }

    /// Counters.
    pub fn stats(&self) -> Stats {
        self.sh.snapshot()
    }

    /// The host clock now, in ns: the clock of the stamps and of
    /// [`ClockSnapshot::out_frame_at`]. `None` while the streams are closed (after a
    /// device event, until [`AudioEngine::reopen`]). Call it on the thread that owns the
    /// engine, outside the callbacks.
    pub fn host_now_ns(&self) -> Option<u64> {
        self.streams.as_ref().map(HostClock::host_now_ns)
    }

    /// The stamp of the last output block, readable from any thread without a lock. The
    /// same playhead serves every epoch: after a reopen it returns the stamps of the new
    /// streams, with the new epoch.
    pub fn playhead(&self) -> Playhead {
        self.playhead.clone()
    }

    /// Parameters of the open (or last open) streams.
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Epoch of the clock map.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The clock map, for inspection (only in [`Pump::Manual`], where this thread owns it).
    pub fn clock_map(&self) -> Option<&ClockMap> {
        self.analysis.as_ref().map(|a| &a.clock)
    }

    /// The calibration no longer applies (a device event happened since it was made).
    pub fn calibration_stale(&self) -> bool {
        self.sh.calib_stale.load(Ordering::Acquire)
    }

    /// The calibration code calls this after a fresh calibration or a passed quick check.
    pub fn clear_calibration_stale(&self) {
        self.sh.calib_stale.store(false, Ordering::Release);
    }

    /// Opens the streams again after a device event (or any time): new epoch, clock map
    /// reset, calibration stale. Returns fresh handles for the new mixer and tap; the
    /// event queue stays. Handles from before are dead (their `send` refuses). The rates
    /// may differ from before (the sound bank is regenerated if needed).
    ///
    /// Nothing is committed unless everything succeeds: on an error the engine is closed
    /// but `reopen` can be called again.
    pub fn reopen(&mut self) -> Result<(MixerHandle, TapHandle), AudioError> {
        self.halt_analysis();
        drop(self.streams.take());
        if let Some(mut a) = self.analysis.take() {
            Self::drain_final(&mut a);
            self.link = Some(a.link);
        }
        let link = self.link.take().ok_or_else(|| {
            AudioError::Backend("engine has no analysis state to reopen".to_string())
        })?;
        self.stop = Arc::new(AtomicBool::new(false));
        let worker = match self.mode {
            Pump::Manual => None,
            Pump::Thread => match spawn_worker(self.stop.clone(), self.cfg.wake) {
                Ok(w) => Some(w),
                Err(e) => {
                    self.link = Some(link);
                    return Err(e);
                }
            },
        };
        let opened = match open_pipe(
            &self.backend,
            &self.cfg,
            &self.bank,
            &self.sh,
            &self.playhead,
            self.epoch + 1,
        ) {
            Ok(o) => o,
            Err(e) => {
                // Nothing was published: the callbacks of streams that failed to open
                // never got their stage.
                self.link = Some(link);
                return Err(e);
            }
        };
        self.epoch += 1;
        self.sh.epoch.store(self.epoch, R);
        self.playhead.set_epoch(self.epoch);
        self.sh.calib_stale.store(true, Ordering::Release);
        self.bank = opened.bank;
        self.info = opened.info.clone();
        self.streams = Some(opened.streams);
        let a = Analysis::new(
            self.sh.clone(),
            &self.cfg,
            link,
            opened.rings,
            &opened.info,
            self.epoch,
        );
        self.install(a, worker);
        Ok((opened.mix, opened.tap))
    }

    fn shutdown(&mut self) {
        drop(self.streams.take());
        self.halt_analysis();
        if let Some(a) = self.analysis.as_mut() {
            Self::drain_final(a);
        }
    }

    /// Stops the streams and the thread, flushes the open onset group, drains what is
    /// left into the event queue and returns the final counters.
    pub fn stop(mut self) -> Stats {
        self.shutdown();
        self.stats()
    }
}

impl<B: Backend> Drop for AudioEngine<B> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeBackend, FakeConfig};
    use guitarness_dsp::Strum;

    const BASE_IN: u64 = 5_000_000_000;
    const BASE_OUT: u64 = 5_000_000_000;
    const BLOCK_NS: u64 = 10_000_000;

    #[test]
    fn config_default_is_plan() {
        let c = EngineConfig::default();
        assert_eq!(c.event_ring, 4096);
        assert_eq!(c.cmd_ring, 256);
        assert_eq!(c.wake, Duration::from_millis(1));
        assert!((c.glitch_before_s - 0.1).abs() < 1e-12);
        assert!((c.glitch_after_s - 0.25).abs() < 1e-12);
    }

    /// An analysis side fed by hand: 48 kHz both ways, 480-frame blocks.
    struct Rig {
        sh: Arc<Shared>,
        a: Analysis,
        out_tx: Producer<Stamp>,
        in_tx: Producer<InEvent>,
        pitch_tx: Producer<Frame>,
        note_tx: Option<Producer<Note>>,
        _back_tx: Producer<Box<InStage>>,
        ev: Consumer<EngineEvent>,
    }

    fn rig(event_cap: usize, note_cap: usize, in_cap: usize) -> Rig {
        let sh = Arc::new(Shared::default());
        let (out_tx, out_stamps) = RingBuffer::new(8192);
        let (in_tx, in_events) = RingBuffer::new(in_cap);
        let (pitch_tx, pitch) = RingBuffer::new(4096);
        let (_mix_tx, mix_events) = RingBuffer::<MixEvent>::new(16);
        let (note_tx, notes) = RingBuffer::new(note_cap);
        let (back_tx, stage_back) = RingBuffer::new(1);
        let (events, ev) = RingBuffer::new(event_cap);
        let info = StreamInfo {
            fs_in: 48_000,
            fs_out: 48_000,
            ch_in: 1,
            ch_out: 2,
            in_id: String::new(),
            out_id: String::new(),
            origin_offset_ns: 0,
        };
        let a = Analysis::new(
            sh.clone(),
            &EngineConfig::default(),
            Link {
                events,
                reported: [0; 8],
            },
            Rings {
                out_stamps,
                in_events,
                pitch,
                mix_events,
                notes,
                stage_back,
            },
            &info,
            0,
        );
        Rig {
            sh,
            a,
            out_tx,
            in_tx,
            pitch_tx,
            note_tx: Some(note_tx),
            _back_tx: back_tx,
            ev,
        }
    }

    fn stamp(k: u64, base: u64, extra_ns: u64) -> Stamp {
        let ns = base + k * BLOCK_NS + extra_ns;
        Stamp {
            first: k * 480,
            frames: 480,
            device_ns: ns,
            callback_ns: ns + 2000,
        }
    }

    fn in_block(r: &mut Rig, k: u64, extra_ns: u64) {
        let ev = InEvent::Block {
            stamp: stamp(k, BASE_IN, extra_ns),
            det_first: k * 480,
        };
        assert!(r.in_tx.push(ev).is_ok());
    }

    fn out_block(r: &mut Rig, k: u64, base: u64, extra_ns: u64) {
        assert!(r.out_tx.push(stamp(k, base, extra_ns)).is_ok());
    }

    fn strum(t: f64) -> InEvent {
        InEvent::Onset(OnsetEvent::Strum(Strum {
            t,
            spread: 0.0,
            peaks: 1,
            strength: 2.0,
            rise_db: 12.0,
        }))
    }

    fn events(r: &mut Rig) -> Vec<EngineEvent> {
        let mut v = Vec::new();
        while let Ok(e) = r.ev.pop() {
            v.push(e);
        }
        v
    }

    fn xrun_note(ev: BackendEvent, in_end: u64, out_end: u64) -> Note {
        Note {
            ev,
            at: None,
            in_end,
            out_end,
        }
    }

    /// Finding 3 (overflow): more pending notices than the old 16-slot table must keep
    /// their boundary instead of being applied at once.
    #[test]
    fn notices_beyond_sixteen_keep_their_boundary() {
        let mut r = rig(4096, 64, 4096);
        for k in 0..300 {
            in_block(&mut r, k, 0);
        }
        r.a.step();
        let tx = r.note_tx.as_mut().unwrap();
        for _ in 0..30 {
            assert!(
                tx.push(xrun_note(BackendEvent::XrunIn, 10_000_000, 0))
                    .is_ok()
            );
        }
        r.a.step();
        assert_eq!(r.a.clock.stats(Side::In).xrun_notices, 0);
        assert_eq!(r.a.notes.len(), 30);
    }

    /// Pulso A1: a note read after its stamp went through the map. With the boundary of
    /// the xrun the step is still on that stamp; without it, on the next one.
    #[test]
    fn a_late_note_is_placed_by_its_boundary() {
        use crate::clock::StepCause;
        let steps = |ev: &[EngineEvent]| -> Vec<ClockStep> {
            ev.iter()
                .filter_map(|e| match e {
                    EngineEvent::Step(s) => Some(*s),
                    _ => None,
                })
                .collect()
        };
        for with_boundary in [true, false] {
            let mut r = rig(4096, 64, 4096);
            for k in 0..600 {
                in_block(&mut r, k, 0);
                out_block(&mut r, k, BASE_OUT, 0);
            }
            r.a.step();
            events(&mut r);
            // The input loses 1.5 ms before block 600. Two blocks are analysed before
            // the note is read.
            in_block(&mut r, 600, 1_500_000);
            in_block(&mut r, 601, 1_500_000);
            r.a.step();
            assert!(steps(&events(&mut r)).is_empty());
            let note = Note {
                ev: BackendEvent::XrunIn,
                at: with_boundary.then_some(XrunBoundary::Exact(600 * 480)),
                in_end: 602 * 480,
                out_end: 0,
            };
            assert!(r.note_tx.as_mut().unwrap().push(note).is_ok());
            r.a.step();
            let ev = events(&mut r);
            let found = steps(&ev);
            if with_boundary {
                assert_eq!(found.len(), 1, "{ev:?}");
                let s = found[0];
                assert_eq!(
                    (s.side, s.frame, s.cause),
                    (Side::In, 600 * 480, StepCause::Notice)
                );
                assert!((s.step_s - 1.5e-3).abs() < 1.0e-6, "{s:?}");
                let window = ev.iter().find_map(|e| match e {
                    EngineEvent::Unjudged { from_in, to_in, .. } => Some((*from_in, *to_in)),
                    _ => None,
                });
                let c = 600.0 * 480.0;
                assert_eq!(window, Some((c - 4800.0, c + 12_000.0)));
                assert_eq!(r.a.clock.stats(Side::In).xrun_notices, 1);
            } else {
                // Held for the first stamp after what the engine had seen.
                assert!(found.is_empty(), "{ev:?}");
                in_block(&mut r, 602, 1_500_000);
                r.a.step();
                let found = steps(&events(&mut r));
                assert_eq!(found.len(), 1);
                assert_eq!(
                    (found[0].frame, found[0].cause),
                    (602 * 480, StepCause::Notice)
                );
                assert!(found[0].step_s < 1.5e-3 - 1.0e-5, "{:?}", found[0]);
            }
            // A second note for a stamp that is still to come waits for it.
            let note = Note {
                ev: BackendEvent::XrunIn,
                at: Some(XrunBoundary::Exact(700 * 480)),
                in_end: 603 * 480,
                out_end: 0,
            };
            assert!(r.note_tx.as_mut().unwrap().push(note).is_ok());
            r.a.step();
            assert_eq!(r.a.notes.len(), 1);
        }
    }

    fn steps_of(ev: &[EngineEvent]) -> Vec<ClockStep> {
        ev.iter()
            .filter_map(|e| match e {
                EngineEvent::Step(s) => Some(*s),
                _ => None,
            })
            .collect()
    }

    fn windows_of(ev: &[EngineEvent]) -> Vec<(f64, f64)> {
        ev.iter()
            .filter_map(|e| match e {
                EngineEvent::Unjudged { from_in, to_in, .. } => Some((*from_in, *to_in)),
                _ => None,
            })
            .collect()
    }

    /// A rig locked on 600 blocks of both streams.
    fn locked_rig() -> Rig {
        let mut r = rig(1 << 14, 2048, 4096);
        for k in 0..600 {
            in_block(&mut r, k, 0);
            out_block(&mut r, k, BASE_OUT, 0);
        }
        r.a.step();
        assert_eq!(r.a.clock.quality(), ClockQuality::Locked);
        events(&mut r);
        r
    }

    /// A1 review, finding 3, end to end: the cpal latch, the engine's error callback and
    /// the analysis. The input loses 1.5 ms before block 600 and 1.0 ms more before
    /// block 604; as on WASAPI capture, each error is raised right before the data
    /// callback of the block after the loss. The dispatcher runs late, after block 607,
    /// and delivers both in one drain. Each xrun keeps its own boundary, so each loss
    /// gets its own `Notice` step and window. (The latch used to keep only the first
    /// boundary; the second came with none and was put where the engine stood.)
    #[test]
    fn clustered_xruns_of_the_cpal_latch_each_keep_their_boundary() {
        use crate::clock::StepCause;
        use crate::cpal_backend::{Delivered, EventLatch};
        let mut r = locked_rig();
        let latch = EventLatch::new();
        latch.set_progress(Side::In, 600 * 480);
        for k in 600..608 {
            if k == 600 || k == 604 {
                latch.post(Side::In, BackendEvent::XrunIn);
            }
            let extra = if k < 604 { 1_500_000 } else { 2_500_000 };
            in_block(&mut r, k, extra);
            latch.set_progress(Side::In, (k + 1) * 480);
            out_block(&mut r, k, BASE_OUT, 0);
        }
        r.a.step();
        assert!(steps_of(&events(&mut r)).is_empty());
        let mut cb = error_cb(r.sh.clone(), r.note_tx.take().unwrap());
        latch.drain(&mut Delivered::default(), &mut *cb);
        r.a.step();
        let ev = events(&mut r);
        let found = steps_of(&ev);
        assert_eq!(found.len(), 2, "{ev:?}");
        for (s, (block, size)) in found.iter().zip([(600u64, 1.5e-3), (604, 1.0e-3)]) {
            assert_eq!(
                (s.side, s.frame, s.cause),
                (Side::In, block * 480, StepCause::Notice)
            );
            assert!((s.step_s - size).abs() < 1.0e-6, "{s:?}");
        }
        let c = |block: u64| (block * 480) as f64;
        assert_eq!(
            windows_of(&ev),
            [
                (c(600) - 4800.0, c(600) + 12_000.0),
                (c(604) - 4800.0, c(604) + 12_000.0)
            ]
        );
    }

    /// A lost boundary (the latch's ring was full) is not guessed: the span it may lie in,
    /// up to where the line is re-anchored, is not judged, and no step claims a place
    /// inside it.
    #[test]
    fn a_lost_boundary_leaves_a_span_and_no_guess() {
        use crate::clock::StepCause;
        let mut r = locked_rig();
        for k in 600..608 {
            in_block(&mut r, k, 1_500_000);
            out_block(&mut r, k, BASE_OUT, 0);
        }
        r.a.step();
        events(&mut r);
        let note = Note {
            ev: BackendEvent::XrunIn,
            at: Some(XrunBoundary::Within {
                from: 598 * 480,
                to: 603 * 480,
            }),
            in_end: 608 * 480,
            out_end: 0,
        };
        assert!(r.note_tx.as_mut().unwrap().push(note).is_ok());
        r.a.step();
        let ev = events(&mut r);
        assert!(steps_of(&ev).is_empty(), "{ev:?}");
        let c = |block: u64| (block * 480) as f64;
        assert_eq!(windows_of(&ev), [(c(598) - 4800.0, c(608) + 12_000.0)]);
        // The next stamp re-anchors the line: a step there, with its own window.
        in_block(&mut r, 608, 1_500_000);
        r.a.step();
        let ev = events(&mut r);
        let found = steps_of(&ev);
        assert_eq!(found.len(), 1, "{ev:?}");
        assert_eq!(
            (found[0].frame, found[0].cause),
            (608 * 480, StepCause::Notice)
        );
        assert_eq!(windows_of(&ev), [(c(608) - 4800.0, c(608) + 12_000.0)]);
    }

    /// A1 review, finding 1, in the engine: a note later than the clock map's history
    /// leaves the span from its boundary to the last stamp pushed, not a step at a later
    /// stamp.
    #[test]
    fn a_note_beyond_the_map_history_leaves_a_span() {
        use crate::clock::UNDO_STAMPS;
        let mut r = locked_rig();
        let last = 600 + UNDO_STAMPS as u64 + 5;
        for k in 600..last {
            in_block(&mut r, k, 1_500_000);
            out_block(&mut r, k, BASE_OUT, 0);
        }
        r.a.step();
        events(&mut r);
        let note = Note {
            ev: BackendEvent::XrunIn,
            at: Some(XrunBoundary::Exact(600 * 480)),
            in_end: last * 480,
            out_end: 0,
        };
        assert!(r.note_tx.as_mut().unwrap().push(note).is_ok());
        r.a.step();
        let ev = events(&mut r);
        assert!(steps_of(&ev).is_empty(), "{ev:?}");
        let c = |block: u64| (block * 480) as f64;
        assert_eq!(windows_of(&ev), [(c(600) - 4800.0, c(last) + 12_000.0)]);
        // The notice is armed for the next stamp (a step there only if enough of the
        // loss is left; this young loop has absorbed most of it), never before it.
        in_block(&mut r, last, 1_500_000);
        r.a.step();
        let found = steps_of(&events(&mut r));
        assert!(found.iter().all(|s| s.frame >= last * 480), "{found:?}");
        assert_eq!(r.a.clock.stats(Side::In).xrun_notices, 1);
    }

    /// Notices waiting for their stamp beyond the table's size leave a window each.
    #[test]
    fn held_notices_that_overflow_leave_windows() {
        let mut r = locked_rig();
        let tx = r.note_tx.as_mut().unwrap();
        let n = NOTES as u64 + 6;
        for k in 0..n {
            let note = Note {
                ev: BackendEvent::XrunIn,
                at: Some(XrunBoundary::Exact(10_000_000 + k)),
                in_end: 600 * 480,
                out_end: 0,
            };
            assert!(tx.push(note).is_ok());
        }
        r.a.step();
        assert_eq!(r.a.notes.len(), NOTES);
        let w = windows_of(&events(&mut r));
        assert_eq!(w.len(), 6);
        assert_eq!(w[0], (10_000_000.0 - 4800.0, 10_000_000.0 + 12_000.0));
    }

    /// Finding 3 (ordering): a producer pushes `note, stamp` pairs while the analysis
    /// steps concurrently. Every sub-3 ms discontinuity must become a `Notice` step: the
    /// note is always read before the stamp it belongs to.
    #[test]
    fn note_is_never_applied_after_its_stamp() {
        let mut r = rig(1 << 17, 4096, 4096);
        let mut tx = r.note_tx.take().unwrap();
        let mut in_tx = std::mem::replace(&mut r.in_tx, RingBuffer::new(1).0);
        let blocks = 30_000u64;
        let producer = std::thread::spawn(move || {
            let mut extra: i64 = 0;
            let mut jumps = 0u64;
            for k in 0..blocks {
                if k >= 400 && k % 10 == 0 {
                    extra += if jumps.is_multiple_of(2) {
                        1_500_000
                    } else {
                        -1_500_000
                    };
                    jumps += 1;
                    while tx
                        .push(xrun_note(BackendEvent::XrunIn, k * 480, 0))
                        .is_err()
                    {
                        std::thread::yield_now();
                    }
                }
                let ev = InEvent::Block {
                    stamp: stamp(k, BASE_IN, extra as u64),
                    det_first: k * 480,
                };
                while in_tx.push(ev).is_err() {
                    std::thread::yield_now();
                }
            }
            jumps
        });
        let mut steps = 0u64;
        while !producer.is_finished() || r.a.rings.in_events.slots() > 0 {
            r.a.step();
            steps += events(&mut r)
                .iter()
                .filter(|e| matches!(e, EngineEvent::Step(_)))
                .count() as u64;
        }
        let jumps = producer.join().unwrap();
        r.a.step();
        steps += events(&mut r)
            .iter()
            .filter(|e| matches!(e, EngineEvent::Step(_)))
            .count() as u64;
        assert_eq!(steps, jumps, "a discontinuity was absorbed without a step");
    }

    /// Finding 4: an output step is processed in the same pass as the input stamps that
    /// define the input line; the window must use that line.
    #[test]
    fn output_window_uses_the_input_line_of_the_same_pass() {
        let mut r = rig(4096, 64, 4096);
        // The input device clock started 0.5 s later than the output one.
        let in_base = BASE_OUT + 500_000_000;
        for k in 0..400 {
            let extra = if k >= 300 { 5_000_000 } else { 0 };
            out_block(&mut r, k, BASE_OUT, extra);
            let ev = InEvent::Block {
                stamp: stamp(k, in_base, 0),
                det_first: k * 480,
            };
            assert!(r.in_tx.push(ev).is_ok());
        }
        assert!(
            r.note_tx
                .as_mut()
                .unwrap()
                .push(xrun_note(BackendEvent::XrunOut, 0, 300 * 480))
                .is_ok()
        );
        r.a.step();
        let ev = events(&mut r);
        let step = ev
            .iter()
            .find_map(|e| match e {
                EngineEvent::Step(s) if s.side == Side::Out => Some(*s),
                _ => None,
            })
            .expect("output step");
        let (lo, _) = ev
            .iter()
            .find_map(|e| match e {
                EngineEvent::Unjudged { from_in, to_in, .. } => Some((*from_in, *to_in)),
                _ => None,
            })
            .expect("window");
        // Output frame f plays at host time f/fs (+5 ms after the step); input frame g is
        // captured at 0.5 s + g/fs. So in_of_out(f) = f + 240 - 24000.
        let center = lo + 0.1 * 48_000.0;
        let want = step.frame as f64 + 240.0 - 24_000.0;
        assert!((center - want).abs() < 60.0, "center {center}, want {want}");
    }

    /// Finding 5: a dropped Block event across an input gap.
    #[test]
    fn dropped_block_event_does_not_misplace_onsets_or_pitch() {
        let mut r = rig(4096, 64, 4096);
        in_block(&mut r, 0, 0);
        // Block 1 (detector 480..960) is dropped by a full ring; the frames of the input
        // gap (960 lost) make block 2 start at input frame 1920.
        assert!(r.in_tx.push(strum(700.0)).is_ok());
        let ns = BASE_IN + 3 * BLOCK_NS;
        let b2 = Stamp {
            first: 1920,
            frames: 480,
            device_ns: ns,
            callback_ns: ns + 2000,
        };
        assert!(
            r.in_tx
                .push(InEvent::Block {
                    stamp: b2,
                    det_first: 960
                })
                .is_ok()
        );
        let f = Frame {
            t: 700.0 / 48_000.0,
            ..Frame::default()
        };
        assert!(r.pitch_tx.push(f).is_ok());
        r.a.step();
        let ev = events(&mut r);
        let on = ev
            .iter()
            .find_map(|e| match e {
                EngineEvent::Onset(n) => Some(*n),
                _ => None,
            })
            .expect("onset delivered");
        assert!(on.t_in.is_nan() && !on.judged, "{on:?}");
        assert!(!ev.iter().any(|e| matches!(e, EngineEvent::Pitch(_))));
        assert_eq!(r.sh.snapshot().drops.pitch, 1);
    }

    /// Heard is emitted in the same step as the block that produced it: no extra callback.
    #[test]
    fn heard_is_emitted_in_the_same_pass_as_its_block() {
        let mut r = rig(4096, 64, 4096);
        in_block(&mut r, 0, 0);
        in_block(&mut r, 1, 0);
        r.a.step();
        events(&mut r);
        in_block(&mut r, 2, 0);
        let t = 2.0 * 480.0 + 100.0;
        assert!(
            r.in_tx
                .push(InEvent::Onset(OnsetEvent::Heard { t, at: t + 20.0 }))
                .is_ok()
        );
        r.a.step();
        let on = events(&mut r)
            .into_iter()
            .find_map(|e| match e {
                EngineEvent::Onset(n) => Some(n),
                _ => None,
            })
            .expect("Heard delivered in the same pass");
        assert_eq!(on.t_in, t, "{on:?}");
        assert!(matches!(on.event, OnsetEvent::Heard { .. }));
    }

    /// Finding 6: a full note queue and a full event queue must not lose `Stopped`.
    #[test]
    fn stopped_survives_full_queues() {
        let mut r = rig(2, 1, 4096);
        let mut cb = error_cb(r.sh.clone(), r.note_tx.take().unwrap());
        cb(BackendEvent::Other, None);
        cb(BackendEvent::Other, None); // note queue (1 slot) is full: dropped
        cb(BackendEvent::DeviceLost, None); // dropped too, but latched
        // Fill the event queue so that nothing fits.
        while r
            .a
            .link
            .events
            .push(EngineEvent::Backend(BackendEvent::Other))
            .is_ok()
        {}
        r.a.step();
        assert!(r.ev.pop().is_ok());
        assert!(r.ev.pop().is_ok());
        r.a.step();
        let ev = events(&mut r);
        assert!(
            ev.contains(&EngineEvent::Stopped(BackendEvent::DeviceLost)),
            "{ev:?}"
        );
    }

    /// Finding 8: more windows than the old fixed table in one pass; the onset that falls
    /// in the first one is still inside a window when it is evaluated.
    #[test]
    fn a_backlog_of_steps_keeps_every_window() {
        let mut r = rig(8192, 64, 8192);
        let mut extra = 0u64;
        for k in 0..1000u64 {
            if k >= 400 && k % 50 == 0 {
                extra += 5_000_000;
                let tx = r.note_tx.as_mut().unwrap();
                assert!(tx.push(xrun_note(BackendEvent::XrunIn, k * 480, 0)).is_ok());
            }
            in_block(&mut r, k, extra);
        }
        assert!(r.in_tx.push(strum(400.0 * 480.0 + 100.0)).is_ok());
        in_block(&mut r, 1000, extra);
        r.a.step();
        let ev = events(&mut r);
        let windows = ev
            .iter()
            .filter(|e| matches!(e, EngineEvent::Unjudged { .. }))
            .count();
        assert!(windows >= 10, "{windows} windows");
        let on = ev
            .iter()
            .find_map(|e| match e {
                EngineEvent::Onset(n) => Some(*n),
                _ => None,
            })
            .unwrap();
        assert!(r.a.in_window(on.t_in), "first window was overwritten");
        assert!(!on.judged);
    }

    /// Finding 11: the pitch drop counter must not move backwards when the input callback
    /// publishes its own count.
    #[test]
    fn pitch_drops_from_both_sides_add_up() {
        let mut r = rig(4096, 64, 4096);
        // No block is ever known, so every frame waits: 1100 overflow the 1024 queue.
        for _ in 0..1100 {
            assert!(r.pitch_tx.push(Frame::default()).is_ok());
        }
        r.a.step();
        let mine = r.sh.snapshot().drops.pitch;
        assert_eq!(mine, 76);
        // The callback publishes its (unrelated) count.
        r.sh.drop_pitch.store(0, R);
        assert_eq!(r.sh.snapshot().drops.pitch, 76);
        r.sh.drop_pitch.store(5, R);
        assert_eq!(r.sh.snapshot().drops.pitch, 81);
    }

    /// Finding 16: counters that wrapped still produce notices.
    #[test]
    fn drop_notices_survive_counter_wrap() {
        let mut r = rig(4096, 64, 4096);
        r.sh.drop_cmds.store(u64::MAX - 1, R);
        r.a.link.reported[0] = u64::MAX - 1;
        r.sh.drop_cmds.fetch_add(3, R); // wraps to 1
        r.a.step();
        let ev = events(&mut r);
        assert!(
            ev.contains(&EngineEvent::Dropped {
                ring: Ring::MixCmds,
                n: 3
            }),
            "{ev:?}"
        );
    }

    /// Finding 12: a failed thread spawn in `reopen` commits nothing; the next reopen works.
    #[test]
    fn reopen_is_transactional() {
        let backend = FakeBackend::new(FakeConfig::default());
        let h = backend.handle();
        let (mut eng, _io) = AudioEngine::start(
            backend,
            EngineConfig::default(),
            SoundBank::new(48_000.0),
            Pump::Thread,
        )
        .unwrap();
        FAIL_SPAWN.store(true, Ordering::Relaxed);
        let err = eng.reopen();
        FAIL_SPAWN.store(false, Ordering::Relaxed);
        assert!(err.is_err());
        assert_eq!(eng.epoch(), 0, "epoch committed by a failed reopen");
        assert!(
            !h.is_open(),
            "streams of a failed reopen must not stay open"
        );
        assert!(eng.reopen().is_ok());
        assert_eq!(eng.epoch(), 1);
        assert!(h.is_open());
        let _ = eng.stop();
    }
}
