//! `cpal` 0.18.2 backend (WASAPI shared mode on Windows, CoreAudio on macOS).
//!
//! This is the only module of the crate that imports `cpal`.
//!
//! Rules the callbacks follow (the audio-backend plan section 1): no allocation, no locks
//! that can block, no printing. Scratch buffers are allocated before the
//! streams start. A block larger than the scratch buffer is processed in
//! chunks; each chunk gets its own [`Stamp`].
//!
//! # Time base
//!
//! `Stamp::device_ns` is the `capture` (input) or `playback` (output)
//! `StreamInstant`; `Stamp::callback_ns` is the `callback` instant, both as
//! nanoseconds since the `StreamInstant` origin. cpal does not promise that
//! two streams share an origin. After building both streams the backend reads
//! `out.now()`, `in.now()`, `out.now()`. If the input reading differs from
//! the mean of the two output readings by more than 1 ms, the origins are
//! treated as separate: `StreamInfo::origin_offset_ns` is that difference
//! (`out - in`) and it is added to every input timestamp, so input and
//! output stamps live on the output clock. Otherwise the offset is 0.
//! The triple is read up to 8 times and the one with the narrowest
//! `out_b - out_a` bracket wins. If even that bracket is wider than 200 us
//! (the thread was preempted), the offset is 0 and
//! [`CpalStreams::origin_verified`] returns `false`.
//!
//! # Errors from the stream threads
//!
//! The cpal error callbacks only touch atomics (`EventLatch`). A
//! non-real-time dispatcher thread, owned by [`CpalStreams`], calls the
//! user's `ErrorFn` once per counted event, up to a few milliseconds later.
//! No event is dropped.
//!
//! # Xrun boundary
//!
//! Each data callback publishes its running frame counter (`frames_done`) in
//! the latch. When cpal reports an xrun, the error callback copies the counter
//! of that stream into the latch as the boundary of the xrun, before it counts
//! the event; the dispatcher hands it to the `ErrorFn` with the event. The
//! boundary is the `Stamp::first` of the next block of that stream, so the
//! client can place the xrun however late the dispatcher runs. Every xrun
//! keeps its own boundary, in order, in a preallocated ring of atomics per
//! stream (64 slots; cpal serializes the error callback of each stream, so
//! each ring has one producer). If the dispatcher falls so far behind that a
//! stream's ring is full, the xruns that do not fit are still delivered, with
//! [`XrunBoundary::Within`]: the span between the latest exact boundary and
//! the frame counter at delivery. The client marks that span "not judged"
//! instead of guessing a place in it.
//!
//! How exact the boundary is depends on where cpal raises the error:
//!
//! - WASAPI capture: on the stream thread, right before the data callback of
//!   the packet that follows the lost data (`DATA_DISCONTINUITY`). Exact.
//! - WASAPI render: underruns are not reported at all. The client finds them
//!   from the stamps.
//! - CoreAudio: `kAudioDeviceProcessorOverload` arrives on a listener thread,
//!   not on the IO thread, and for the device, not for one direction. The
//!   boundary is the counter at that moment: the block where the overload
//!   showed up or the next one. Approximate.
//!
//! # Host clock
//!
//! [`CpalStreams`] implements [`HostClock`] with the output stream's `now()`:
//! a raw read of the clock the stamps are on (QPC on WASAPI,
//! `mach_absolute_time` on CoreAudio).
//!
//! # Digital loopback
//!
//! On WASAPI, an input stream opened on a render (output) device captures
//! what that device plays (`AUDCLNT_STREAMFLAGS_LOOPBACK`, set by cpal when
//! the device is a render endpoint). Pass such a device id as
//! `OpenRequest::input_id`; the backend then takes the render device's mix
//! format as the input format. Windows only; elsewhere it returns an error.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Device, DeviceId, ErrorKind, InputCallbackInfo, OutputCallbackInfo, SampleFormat,
    SizedSample, Stream, StreamConfig, StreamInstant,
};

use crate::backend::{Backend, ErrorFn, HostClock, InputFn, IoCallbacks, OutputFn, XrunBoundary};
use crate::types::{AudioError, BackendEvent, DeviceInfo, OpenRequest, Side, Stamp, StreamInfo};

/// Scratch buffer size in frames, fixed. Larger callbacks are chunked.
const SCRATCH_FRAMES: usize = 8192;
/// Accepted range for `OpenRequest::buffer_frames`.
const BUFFER_FRAMES_RANGE: std::ops::RangeInclusive<u32> = 16..=8192;
/// Widest `out_b - out_a` bracket for which the origin offset is trusted.
const ORIGIN_MAX_BRACKET_NS: u64 = 200_000;
/// Origin reads (out, in, out) tried at most this many times.
const ORIGIN_TRIES: usize = 8;
/// Dispatcher poll period.
const DISPATCH_PERIOD: Duration = Duration::from_millis(2);
/// Input/output origins closer than this are treated as the same clock.
const ORIGIN_TOLERANCE_NS: i64 = 1_000_000;
/// How long the backend waits for a stream to initialise.
const BUILD_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Pure helpers (no device needed)
// ---------------------------------------------------------------------------

/// `StreamInstant` as nanoseconds, saturating at `u64::MAX` (585 years).
pub(crate) fn instant_ns(t: StreamInstant) -> u64 {
    u64::try_from(t.as_nanos()).unwrap_or(u64::MAX)
}

/// Adds a signed offset to a timestamp without overflow or wrap-around.
pub(crate) fn apply_offset(ns: u64, offset: i64) -> u64 {
    if offset >= 0 {
        ns.saturating_add(offset as u64)
    } else {
        ns.saturating_sub(offset.unsigned_abs())
    }
}

/// Origin offset from the three `now()` reads (`out_a`, `in_now`, `out_b`).
/// Returns `out_mean - in_now`, or 0 when it is within 1 ms.
pub(crate) fn origin_offset_ns(out_a: u64, in_now: u64, out_b: u64) -> i64 {
    let mean = (u128::from(out_a) + u128::from(out_b)) / 2;
    let diff = mean as i128 - i128::from(in_now);
    let diff = diff.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
    if diff.unsigned_abs() > ORIGIN_TOLERANCE_NS as u64 {
        diff
    } else {
        0
    }
}

/// Picks the narrowest `(out_a, in, out_b)` triple. Returns the offset and
/// whether it is verified (bracket at most 200 us). Unverified: offset 0.
pub(crate) fn best_origin(triples: &[(u64, u64, u64)]) -> (i64, bool) {
    let best = triples.iter().min_by_key(|t| t.2.saturating_sub(t.0));
    match best {
        Some(&(a, i, b)) if b.saturating_sub(a) <= ORIGIN_MAX_BRACKET_NS => {
            (origin_offset_ns(a, i, b), true)
        }
        _ => (0, false),
    }
}

/// Stamp of the chunk that starts `start` frames into a block.
/// `first`, `device_ns` and `callback_ns` describe the whole block.
pub(crate) fn chunk_stamp(
    first: u64,
    device_ns: u64,
    callback_ns: u64,
    start: usize,
    len: usize,
    fs: u32,
) -> Stamp {
    let adv = (start as u128 * 1_000_000_000) / u128::from(fs.max(1));
    let adv = u64::try_from(adv).unwrap_or(u64::MAX);
    Stamp {
        first: first + start as u64,
        frames: len as u32,
        device_ns: device_ns.saturating_add(adv),
        callback_ns,
    }
}

/// Maps a cpal error kind to a backend event for the given stream side.
pub(crate) fn map_error_kind(kind: ErrorKind, side: Side) -> BackendEvent {
    match kind {
        ErrorKind::Xrun => match side {
            Side::In => BackendEvent::XrunIn,
            Side::Out => BackendEvent::XrunOut,
        },
        ErrorKind::StreamInvalidated => BackendEvent::Invalidated,
        ErrorKind::DeviceNotAvailable => BackendEvent::DeviceLost,
        ErrorKind::DeviceChanged => BackendEvent::DeviceChanged,
        ErrorKind::RealtimeDenied => BackendEvent::RealtimeDenied,
        _ => BackendEvent::Other,
    }
}

/// Accepts only 44.1 and 48 kHz.
pub(crate) fn check_rate(hz: u32) -> Result<u32, AudioError> {
    if hz == 44_100 || hz == 48_000 {
        Ok(hz)
    } else {
        Err(AudioError::UnsupportedRate(hz))
    }
}

/// Validates `buffer_frames` before anything is allocated.
pub(crate) fn validate_buffer_frames(n: Option<u32>) -> Result<Option<u32>, AudioError> {
    match n {
        Some(v) if !BUFFER_FRAMES_RANGE.contains(&v) => {
            Err(AudioError::UnsupportedFormat(format!(
                "buffer size {v} frames is outside {}..={}",
                BUFFER_FRAMES_RANGE.start(),
                BUFFER_FRAMES_RANGE.end()
            )))
        }
        other => Ok(other),
    }
}

/// Parses a device id string produced by [`device_id_string`].
pub(crate) fn parse_device_id(s: &str) -> Result<DeviceId, AudioError> {
    DeviceId::from_str(s).map_err(|e| AudioError::NoDevice(format!("bad device id '{s}': {e}")))
}

// --- error latch -----------------------------------------------------------

const N_EVENTS: usize = 7;

fn event_idx(ev: BackendEvent) -> usize {
    match ev {
        BackendEvent::Invalidated => 0,
        BackendEvent::DeviceLost => 1,
        BackendEvent::DeviceChanged => 2,
        BackendEvent::RealtimeDenied => 3,
        BackendEvent::Other => 4,
        BackendEvent::XrunIn => 5,
        BackendEvent::XrunOut => 6,
    }
}

/// Delivery order: severity, terminal events first.
const EVENT_ORDER: [BackendEvent; N_EVENTS] = [
    BackendEvent::Invalidated,
    BackendEvent::DeviceLost,
    BackendEvent::DeviceChanged,
    BackendEvent::RealtimeDenied,
    BackendEvent::Other,
    BackendEvent::XrunIn,
    BackendEvent::XrunOut,
];

/// Xrun boundaries each stream can hold until the dispatcher takes them. More xruns of
/// one stream between two drains lose their exact boundary
/// ([`XrunBoundary::Within`]).
pub(crate) const BOUNDARY_SLOTS: usize = 64;

fn is_xrun(ev: BackendEvent) -> bool {
    matches!(ev, BackendEvent::XrunIn | BackendEvent::XrunOut)
}

/// The boundaries of one stream's xruns, in the order they were posted: a ring of
/// atomics, preallocated. Producer: the error callback of that stream. Consumer: the
/// dispatcher.
struct BoundaryRing {
    /// Boundary of xrun number `seq` in slot `seq % BOUNDARY_SLOTS`.
    at: [AtomicU64; BOUNDARY_SLOTS],
    /// `seq + 1` of the xrun whose boundary the slot holds (0: none yet). Written after
    /// `at`: a reader that sees its own `seq + 1` here sees the right boundary.
    tag: [AtomicU64; BOUNDARY_SLOTS],
    /// Xruns posted (the number of the next one).
    posted: AtomicU64,
    /// Xruns the dispatcher is done with: their slots may be written again.
    taken: AtomicU64,
}

impl BoundaryRing {
    fn new() -> Self {
        Self {
            at: std::array::from_fn(|_| AtomicU64::new(0)),
            tag: std::array::from_fn(|_| AtomicU64::new(0)),
            posted: AtomicU64::new(0),
            taken: AtomicU64::new(0),
        }
    }
}

/// Per-stream, per-kind event counters, the frame counter of each stream and
/// the boundary of every undelivered xrun. Posting is a few atomic operations:
/// no lock, no allocation, no waiting.
pub(crate) struct EventLatch {
    counts: [[AtomicU32; N_EVENTS]; 2],
    /// Frames each stream has handed to its data callback so far.
    progress: [AtomicU64; 2],
    /// Boundaries of the xruns of each stream not drained yet.
    boundaries: [BoundaryRing; 2],
}

/// What the dispatcher has delivered so far (private to it).
#[derive(Default)]
pub(crate) struct Delivered {
    /// Events of each stream and kind.
    seen: [[u32; N_EVENTS]; 2],
    /// Xruns of each stream (the number of the next one to deliver).
    xruns: [u64; 2],
    /// The latest exact boundary delivered for each stream.
    last: [u64; 2],
}

impl EventLatch {
    pub(crate) fn new() -> Self {
        Self {
            counts: Default::default(),
            progress: Default::default(),
            boundaries: [BoundaryRing::new(), BoundaryRing::new()],
        }
    }

    /// The data callback of `side` has delivered `frames` frames in total.
    pub(crate) fn set_progress(&self, side: Side, frames: u64) {
        self.progress[usize::from(side == Side::Out)].store(frames, Ordering::Release);
    }

    /// Records an event raised by the stream of `side`. For an xrun the frame
    /// counter of that stream is its boundary, kept in the next slot of the
    /// stream's ring if the dispatcher has left one free (otherwise the
    /// dispatcher delivers it as lost). The boundary is written before the
    /// count: a drain that sees the event also sees its boundary.
    pub(crate) fn post(&self, side: Side, ev: BackendEvent) {
        let s = usize::from(side == Side::Out);
        if is_xrun(ev) {
            let r = &self.boundaries[s];
            let seq = r.posted.fetch_add(1, Ordering::AcqRel);
            let at = self.progress[s].load(Ordering::Acquire);
            if seq.wrapping_sub(r.taken.load(Ordering::Acquire)) < BOUNDARY_SLOTS as u64 {
                let i = (seq % BOUNDARY_SLOTS as u64) as usize;
                r.at[i].store(at, Ordering::Relaxed);
                r.tag[i].store(seq + 1, Ordering::Release);
            }
        }
        self.counts[s][event_idx(ev)].fetch_add(1, Ordering::Release);
    }

    /// Delivers every event posted since the last drain, most severe kind
    /// first, input side before output side. `d` is the dispatcher's private
    /// state. Every xrun comes with its own boundary, in the order posted:
    /// exact if its slot was kept, otherwise ([`XrunBoundary::Within`]) the
    /// span it must lie in: from the latest exact boundary of that stream to
    /// the stream's frame counter now (frame counters only grow).
    pub(crate) fn drain(
        &self,
        d: &mut Delivered,
        f: &mut dyn FnMut(BackendEvent, Option<XrunBoundary>),
    ) {
        for ev in EVENT_ORDER {
            let k = event_idx(ev);
            for side in 0..2 {
                let now = self.counts[side][k].load(Ordering::Acquire);
                let n = now.wrapping_sub(d.seen[side][k]);
                if n == 0 {
                    continue;
                }
                d.seen[side][k] = now;
                if !is_xrun(ev) {
                    for _ in 0..n {
                        f(ev, None);
                    }
                    continue;
                }
                let r = &self.boundaries[side];
                // Read after the counts: not before any boundary counted here.
                let progress = self.progress[side].load(Ordering::Acquire);
                for _ in 0..n {
                    let seq = d.xruns[side];
                    d.xruns[side] += 1;
                    let i = (seq % BOUNDARY_SLOTS as u64) as usize;
                    let b = if r.tag[i].load(Ordering::Acquire) == seq + 1 {
                        let at = r.at[i].load(Ordering::Relaxed);
                        d.last[side] = at;
                        XrunBoundary::Exact(at)
                    } else {
                        XrunBoundary::Within {
                            from: d.last[side],
                            to: progress.max(d.last[side]),
                        }
                    };
                    f(ev, Some(b));
                }
                r.taken.store(d.xruns[side], Ordering::Release);
            }
        }
    }
}

/// Non-RT thread that forwards latched events to the user's `ErrorFn`.
pub(crate) struct Dispatcher {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Dispatcher {
    pub(crate) fn spawn(latch: Arc<EventLatch>, mut user: ErrorFn) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("tonica-audio-events".into())
            .spawn(move || {
                let mut delivered = Delivered::default();
                loop {
                    let stopping = stop2.load(Ordering::Acquire);
                    latch.drain(&mut delivered, &mut *user);
                    if stopping {
                        break;
                    }
                    std::thread::sleep(DISPATCH_PERIOD);
                }
            })
            .ok();
        Self { stop, handle }
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[inline]
pub(crate) fn i16_to_f32(x: i16) -> f32 {
    f32::from(x) * (1.0 / 32768.0)
}

#[inline]
pub(crate) fn i32_to_f32(x: i32) -> f32 {
    (f64::from(x) * (1.0 / 2_147_483_648.0)) as f32
}

#[inline]
pub(crate) fn f32_to_i16(x: f32) -> i16 {
    // Same scale as `i16_to_f32`: exact round trip. `as` saturates; NaN is 0.
    (x * 32768.0).round() as i16
}

#[inline]
pub(crate) fn f32_to_i32(x: f32) -> i32 {
    (f64::from(x) * 2_147_483_648.0).round() as i32
}

/// Sample types the backend can convert to and from `f32`.
trait Conv: SizedSample + Copy + Send + 'static {
    fn to_f32(self) -> f32;
    fn from_f32(x: f32) -> Self;
    /// `Some` when the data already is `f32` (no copy needed).
    fn as_f32(_data: &[Self]) -> Option<&[f32]> {
        None
    }
    /// `Some` when the data already is `f32` (no copy needed).
    fn as_f32_mut(_data: &mut [Self]) -> Option<&mut [f32]> {
        None
    }
}

impl Conv for f32 {
    fn to_f32(self) -> f32 {
        self
    }
    fn from_f32(x: f32) -> Self {
        x
    }
    fn as_f32(data: &[Self]) -> Option<&[f32]> {
        Some(data)
    }
    fn as_f32_mut(data: &mut [Self]) -> Option<&mut [f32]> {
        Some(data)
    }
}

impl Conv for i16 {
    fn to_f32(self) -> f32 {
        i16_to_f32(self)
    }
    fn from_f32(x: f32) -> Self {
        f32_to_i16(x)
    }
}

impl Conv for i32 {
    fn to_f32(self) -> f32 {
        i32_to_f32(self)
    }
    fn from_f32(x: f32) -> Self {
        f32_to_i32(x)
    }
}

/// Runs `f` inside `assert_no_alloc` when the `rt-audit` feature is on.
#[inline(always)]
fn rt_guard<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(feature = "rt-audit")]
    {
        assert_no_alloc::assert_no_alloc(f)
    }
    #[cfg(not(feature = "rt-audit"))]
    {
        f()
    }
}

/// Input block handler: owns the user callback, scratch buffer and counter.
struct InputState<T: Conv> {
    user: InputFn,
    scratch: Vec<f32>,
    ch: usize,
    fs: u32,
    frames_done: u64,
    offset: Arc<AtomicI64>,
    /// Where `frames_done` is published for the error callback.
    latch: Arc<EventLatch>,
    _t: std::marker::PhantomData<T>,
}

impl<T: Conv> InputState<T> {
    fn new(
        user: InputFn,
        ch: u16,
        fs: u32,
        offset: Arc<AtomicI64>,
        latch: Arc<EventLatch>,
    ) -> Self {
        let ch = usize::from(ch.max(1));
        Self {
            user,
            scratch: vec![0.0; SCRATCH_FRAMES * ch],
            ch,
            fs,
            frames_done: 0,
            offset,
            latch,
            _t: std::marker::PhantomData,
        }
    }

    fn block(&mut self, data: &[T], capture: StreamInstant, callback: StreamInstant) {
        let off = self.offset.load(Ordering::Relaxed);
        let dev_ns = apply_offset(instant_ns(capture), off);
        let cb_ns = apply_offset(instant_ns(callback), off);
        let total = data.len() / self.ch;
        let ch = self.ch;
        let cap = self.scratch.len() / ch;
        let view = T::as_f32(data);
        let mut start = 0;
        while start < total {
            let len = cap.min(total - start);
            let stamp = chunk_stamp(self.frames_done, dev_ns, cb_ns, start, len, self.fs);
            if let Some(f) = view {
                (self.user)(&f[start * ch..(start + len) * ch], stamp);
            } else {
                let src = &data[start * ch..(start + len) * ch];
                let dst = &mut self.scratch[..len * ch];
                for (o, i) in dst.iter_mut().zip(src) {
                    *o = i.to_f32();
                }
                (self.user)(dst, stamp);
            }
            start += len;
        }
        self.frames_done += total as u64;
        self.latch.set_progress(Side::In, self.frames_done);
    }
}

/// Output block handler.
struct OutputState<T: Conv> {
    user: OutputFn,
    scratch: Vec<f32>,
    ch: usize,
    fs: u32,
    frames_done: u64,
    /// Where `frames_done` is published for the error callback.
    latch: Arc<EventLatch>,
    _t: std::marker::PhantomData<T>,
}

impl<T: Conv> OutputState<T> {
    fn new(user: OutputFn, ch: u16, fs: u32, latch: Arc<EventLatch>) -> Self {
        let ch = usize::from(ch.max(1));
        Self {
            user,
            scratch: vec![0.0; SCRATCH_FRAMES * ch],
            ch,
            fs,
            frames_done: 0,
            latch,
            _t: std::marker::PhantomData,
        }
    }

    fn block(&mut self, data: &mut [T], playback: StreamInstant, callback: StreamInstant) {
        // Output stamps stay on the output clock; the origin offset moves the
        // input stamps only.
        let dev_ns = instant_ns(playback);
        let cb_ns = instant_ns(callback);
        let ch = self.ch;
        let total = data.len() / ch;
        let used = total * ch;
        let cap = self.scratch.len() / ch;
        if let Some(f) = T::as_f32_mut(data) {
            let mut start = 0;
            while start < total {
                let len = cap.min(total - start);
                let stamp = chunk_stamp(self.frames_done, dev_ns, cb_ns, start, len, self.fs);
                (self.user)(&mut f[start * ch..(start + len) * ch], stamp);
                start += len;
            }
            f[used..].fill(0.0);
            self.frames_done += total as u64;
            self.latch.set_progress(Side::Out, self.frames_done);
            return;
        }
        let mut start = 0;
        while start < total {
            let len = cap.min(total - start);
            let buf = &mut self.scratch[..len * ch];
            buf.fill(0.0);
            let stamp = chunk_stamp(self.frames_done, dev_ns, cb_ns, start, len, self.fs);
            (self.user)(buf, stamp);
            let dst = &mut data[start * ch..(start + len) * ch];
            for (o, i) in dst.iter_mut().zip(buf.iter()) {
                *o = T::from_f32(*i);
            }
            start += len;
        }
        // Samples past the last whole frame stay silent.
        for s in &mut data[used..] {
            *s = T::from_f32(0.0);
        }
        self.frames_done += total as u64;
        self.latch.set_progress(Side::Out, self.frames_done);
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// The real backend, on the default cpal host.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpalBackend;

impl CpalBackend {
    /// Creates the backend. No device is touched until `devices` or `open`.
    pub fn new() -> Self {
        Self
    }
}

/// The two open cpal streams. Dropping pauses and closes them.
pub struct CpalStreams {
    // Field order is drop order: input first, then output.
    input: Option<Stream>,
    output: Option<Stream>,
    // Dropped last, after the streams can no longer post events.
    dispatcher: Option<Dispatcher>,
    origin_verified: bool,
}

impl CpalStreams {
    /// `true` when the input/output clock origin check produced a trustworthy
    /// reading (a bracket of at most 200 us). If `false`,
    /// `StreamInfo::origin_offset_ns` is 0 and was not verified.
    pub fn origin_verified(&self) -> bool {
        self.origin_verified
    }
}

impl HostClock for CpalStreams {
    /// `now()` of the output stream: the clock of the stamps, read raw.
    fn host_now_ns(&self) -> u64 {
        self.output.as_ref().map_or(0, |s| instant_ns(s.now()))
    }
}

impl Drop for CpalStreams {
    fn drop(&mut self) {
        if let Some(s) = self.input.take() {
            let _ = s.pause();
            drop(s);
        }
        if let Some(s) = self.output.take() {
            let _ = s.pause();
            drop(s);
        }
        // Joins the thread after a final drain.
        drop(self.dispatcher.take());
    }
}

fn map_cpal_error(ctx: &str, e: &cpal::Error) -> AudioError {
    match e.kind() {
        ErrorKind::DeviceNotAvailable | ErrorKind::HostUnavailable => {
            AudioError::NoDevice(format!("{ctx}: {e}"))
        }
        ErrorKind::UnsupportedConfig => AudioError::UnsupportedFormat(format!("{ctx}: {e}")),
        ErrorKind::PermissionDenied => AudioError::Backend(format!(
            "{ctx}: permission denied (check the microphone privacy setting): {e}"
        )),
        _ => AudioError::Backend(format!("{ctx}: {e}")),
    }
}

fn device_id_string(d: &Device) -> Option<String> {
    d.id().ok().map(|id| id.to_string())
}

fn device_name(d: &Device, fallback: &str) -> String {
    d.description()
        .map(|desc| desc.name().to_string())
        .unwrap_or_else(|_| fallback.to_string())
}

/// Resolves `id` (or the default device) for one direction.
fn resolve(
    host: &cpal::Host,
    id: Option<&str>,
    side: Side,
) -> Result<(Device, String), AudioError> {
    let what = if side == Side::In { "input" } else { "output" };
    let dev = match id {
        Some(s) => {
            let parsed = parse_device_id(s)?;
            host.device_by_id(&parsed)
                .ok_or_else(|| AudioError::NoDevice(format!("{what} device '{s}' not found")))?
        }
        None => match side {
            Side::In => host.default_input_device(),
            Side::Out => host.default_output_device(),
        }
        .ok_or_else(|| AudioError::NoDevice(format!("no default {what} device")))?,
    };
    let id_str = dev
        .id()
        .map_err(|e| AudioError::Backend(format!("{what} device id unavailable: {e}")))?
        .to_string();
    Ok((dev, id_str))
}

/// Error callback: a few atomic operations (the count and, for an xrun, the
/// boundary), nothing else.
fn error_sink(latch: Arc<EventLatch>, side: Side) -> impl FnMut(cpal::Error) + Send + 'static {
    move |e: cpal::Error| {
        let ev = map_error_kind(e.kind(), side);
        // Audited like the data callbacks; cpal's error value is dropped outside.
        rt_guard(|| latch.post(side, ev));
    }
}

fn stream_config(base: &cpal::SupportedStreamConfig, buffer_frames: Option<u32>) -> StreamConfig {
    let mut cfg = base.config();
    if let Some(n) = buffer_frames {
        cfg.buffer_size = BufferSize::Fixed(n);
    }
    cfg
}

fn build_input<T: Conv>(
    dev: &Device,
    cfg: StreamConfig,
    user: InputFn,
    offset: Arc<AtomicI64>,
    err: Arc<EventLatch>,
) -> Result<Stream, cpal::Error> {
    let mut st = InputState::<T>::new(user, cfg.channels, cfg.sample_rate, offset, err.clone());
    dev.build_input_stream::<T, _, _>(
        cfg,
        move |data: &[T], info: &InputCallbackInfo| {
            let ts = info.timestamp();
            rt_guard(|| st.block(data, ts.capture, ts.callback));
        },
        error_sink(err, Side::In),
        Some(BUILD_TIMEOUT),
    )
}

fn build_output<T: Conv>(
    dev: &Device,
    cfg: StreamConfig,
    user: OutputFn,
    err: Arc<EventLatch>,
) -> Result<Stream, cpal::Error> {
    let mut st = OutputState::<T>::new(user, cfg.channels, cfg.sample_rate, err.clone());
    dev.build_output_stream::<T, _, _>(
        cfg,
        move |data: &mut [T], info: &OutputCallbackInfo| {
            let ts = info.timestamp();
            rt_guard(|| st.block(data, ts.playback, ts.callback));
        },
        error_sink(err, Side::Out),
        Some(BUILD_TIMEOUT),
    )
}

/// Input config: the device default; on Windows a render device means
/// WASAPI loopback and uses the render mix format.
fn input_config(dev: &Device) -> Result<cpal::SupportedStreamConfig, AudioError> {
    if dev.supports_input() {
        return dev
            .default_input_config()
            .map_err(|e| map_cpal_error("default input config", &e));
    }
    #[cfg(windows)]
    if dev.supports_output() {
        return dev
            .default_output_config()
            .map_err(|e| map_cpal_error("loopback input config", &e));
    }
    Err(AudioError::NoDevice(
        "the selected device cannot capture (digital loopback exists on Windows only)".into(),
    ))
}

fn unsupported_format(f: SampleFormat) -> AudioError {
    AudioError::UnsupportedFormat(format!("{f:?} (accepted: F32, I16, I32)"))
}

impl Backend for CpalBackend {
    type Streams = CpalStreams;

    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        let host = cpal::default_host();
        let def_in = host
            .default_input_device()
            .and_then(|d| device_id_string(&d));
        let def_out = host
            .default_output_device()
            .and_then(|d| device_id_string(&d));
        let list = host
            .devices()
            .map_err(|e| map_cpal_error("enumerate devices", &e))?;
        let mut out = Vec::new();
        for d in list {
            let Some(id) = device_id_string(&d) else {
                continue;
            };
            let is_input = d.supports_input();
            let is_output = d.supports_output();
            let is_default = (is_input && def_in.as_deref() == Some(id.as_str()))
                || (is_output && def_out.as_deref() == Some(id.as_str()));
            out.push(DeviceInfo {
                name: device_name(&d, &id),
                id,
                is_input,
                is_output,
                is_default,
            });
        }
        Ok(out)
    }

    fn open(
        &self,
        req: &OpenRequest,
        io: IoCallbacks,
    ) -> Result<(CpalStreams, StreamInfo), AudioError> {
        let buffer_frames = validate_buffer_frames(req.buffer_frames)?;
        let host = cpal::default_host();
        let (in_dev, in_id) = resolve(&host, req.input_id.as_deref(), Side::In)?;
        let (out_dev, out_id) = resolve(&host, req.output_id.as_deref(), Side::Out)?;

        let in_def = input_config(&in_dev)?;
        let out_def = out_dev
            .default_output_config()
            .map_err(|e| map_cpal_error("default output config", &e))?;
        let fs_in = check_rate(in_def.sample_rate())?;
        let fs_out = check_rate(out_def.sample_rate())?;
        let in_cfg = stream_config(&in_def, buffer_frames);
        let out_cfg = stream_config(&out_def, buffer_frames);

        let offset = Arc::new(AtomicI64::new(0));
        let err = Arc::new(EventLatch::new());

        let output = match out_def.sample_format() {
            SampleFormat::F32 => build_output::<f32>(&out_dev, out_cfg, io.output, err.clone()),
            SampleFormat::I16 => build_output::<i16>(&out_dev, out_cfg, io.output, err.clone()),
            SampleFormat::I32 => build_output::<i32>(&out_dev, out_cfg, io.output, err.clone()),
            f => return Err(unsupported_format(f)),
        }
        .map_err(|e| map_cpal_error("build output stream", &e))?;

        let input = match in_def.sample_format() {
            SampleFormat::F32 => {
                build_input::<f32>(&in_dev, in_cfg, io.input, offset.clone(), err.clone())
            }
            SampleFormat::I16 => {
                build_input::<i16>(&in_dev, in_cfg, io.input, offset.clone(), err.clone())
            }
            SampleFormat::I32 => {
                build_input::<i32>(&in_dev, in_cfg, io.input, offset.clone(), err.clone())
            }
            f => return Err(unsupported_format(f)),
        }
        .map_err(|e| map_cpal_error("build input stream", &e))?;

        // Origin check (the audio-backend plan section 3): out, in, out. Streams are still
        // paused, so no callback reads the offset yet.
        let mut triples = [(0u64, 0u64, 0u64); ORIGIN_TRIES];
        let mut n = 0;
        while n < ORIGIN_TRIES {
            let out_a = instant_ns(output.now());
            let in_now = instant_ns(input.now());
            let out_b = instant_ns(output.now());
            triples[n] = (out_a, in_now, out_b);
            n += 1;
            if out_b.saturating_sub(out_a) <= ORIGIN_MAX_BRACKET_NS / 4 {
                break;
            }
        }
        let (origin_offset, origin_verified) = best_origin(&triples[..n]);
        offset.store(origin_offset, Ordering::Relaxed);

        // Constructing `CpalStreams` first means an error below still closes both.
        let streams = CpalStreams {
            input: Some(input),
            output: Some(output),
            dispatcher: Some(Dispatcher::spawn(err, io.error)),
            origin_verified,
        };
        if let Some(s) = &streams.output {
            s.play()
                .map_err(|e| map_cpal_error("start output stream", &e))?;
        }
        if let Some(s) = &streams.input {
            s.play()
                .map_err(|e| map_cpal_error("start input stream", &e))?;
        }

        let info = StreamInfo {
            fs_in,
            fs_out,
            ch_in: in_def.channels(),
            ch_out: out_def.channels(),
            in_id,
            out_id,
            origin_offset_ns: origin_offset,
        };
        Ok((streams, info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, AtomicU64};

    #[test]
    fn i16_roundtrip_and_limits() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), 32767);
        assert_eq!(f32_to_i16(-1.0), -32768);
        assert_eq!(f32_to_i16(2.5), 32767);
        assert_eq!(f32_to_i16(f32::NAN), 0);
        assert_eq!(i16_to_f32(i16::MIN), -1.0);
        for x in [-32768i16, -1234, -1, 0, 1, 77, 32767] {
            assert_eq!(f32_to_i16(i16_to_f32(x)), x);
        }
    }

    #[test]
    fn i32_roundtrip_and_limits() {
        assert_eq!(f32_to_i32(0.0), 0);
        assert_eq!(f32_to_i32(1.0), i32::MAX);
        assert_eq!(f32_to_i32(-3.0), i32::MIN);
        assert_eq!(i32_to_f32(i32::MIN), -1.0);
        assert!((i32_to_f32(1 << 30) - 0.5).abs() < 1e-7);
        let back = f32_to_i32(i32_to_f32(1 << 20));
        assert!((i64::from(back) - (1 << 20)).abs() < 300);
    }

    #[test]
    fn stamp_arithmetic() {
        let s = chunk_stamp(1000, 5_000_000, 4_000_000, 0, 480, 48_000);
        assert_eq!(
            (s.first, s.frames, s.device_ns, s.callback_ns),
            (1000, 480, 5_000_000, 4_000_000)
        );
        // 480 frames at 48 kHz = 10 ms.
        let s = chunk_stamp(1000, 5_000_000, 4_000_000, 480, 480, 48_000);
        assert_eq!((s.first, s.device_ns), (1480, 15_000_000));
        // 44.1 kHz: 441 frames = 10 ms exactly; large values do not overflow.
        let s = chunk_stamp(0, u64::MAX - 5, 0, 441, 1, 44_100);
        assert_eq!(s.device_ns, u64::MAX);
        let s = chunk_stamp(0, 0, 0, 441, 1, 44_100);
        assert_eq!(s.device_ns, 10_000_000);
    }

    #[test]
    fn offset_math() {
        assert_eq!(apply_offset(10, -20), 0);
        assert_eq!(apply_offset(10, 5), 15);
        assert_eq!(apply_offset(u64::MAX, 5), u64::MAX);
        assert_eq!(apply_offset(1_000, i64::MIN), 0);
        // Within 1 ms: separate origin is not declared.
        assert_eq!(origin_offset_ns(1_000_000, 1_500_000, 1_200_000), 0);
        // Far apart: offset = out_mean - in.
        assert_eq!(
            origin_offset_ns(10_000_000, 4_000_000, 10_000_000),
            6_000_000
        );
        assert_eq!(origin_offset_ns(0, 9_000_000, 0), -9_000_000);
        // Extremes do not panic.
        let _ = origin_offset_ns(u64::MAX, 0, u64::MAX);
    }

    #[test]
    fn rate_check() {
        assert_eq!(check_rate(48_000), Ok(48_000));
        assert_eq!(check_rate(44_100), Ok(44_100));
        assert_eq!(check_rate(96_000), Err(AudioError::UnsupportedRate(96_000)));
    }

    #[test]
    fn error_mapping() {
        use BackendEvent as B;
        assert_eq!(map_error_kind(ErrorKind::Xrun, Side::In), B::XrunIn);
        assert_eq!(map_error_kind(ErrorKind::Xrun, Side::Out), B::XrunOut);
        assert_eq!(
            map_error_kind(ErrorKind::StreamInvalidated, Side::In),
            B::Invalidated
        );
        assert_eq!(
            map_error_kind(ErrorKind::DeviceNotAvailable, Side::Out),
            B::DeviceLost
        );
        assert_eq!(
            map_error_kind(ErrorKind::DeviceChanged, Side::Out),
            B::DeviceChanged
        );
        assert_eq!(
            map_error_kind(ErrorKind::RealtimeDenied, Side::In),
            B::RealtimeDenied
        );
        assert_eq!(map_error_kind(ErrorKind::BackendError, Side::In), B::Other);
        assert_eq!(map_error_kind(ErrorKind::Other, Side::Out), B::Other);
    }

    #[test]
    fn buffer_frames_validation() {
        assert_eq!(validate_buffer_frames(None), Ok(None));
        assert_eq!(validate_buffer_frames(Some(256)), Ok(Some(256)));
        assert_eq!(validate_buffer_frames(Some(16)), Ok(Some(16)));
        assert_eq!(validate_buffer_frames(Some(8192)), Ok(Some(8192)));
        for bad in [0, 15, 8193, u32::MAX] {
            assert!(matches!(
                validate_buffer_frames(Some(bad)),
                Err(AudioError::UnsupportedFormat(_))
            ));
        }
    }

    #[test]
    fn device_id_roundtrip() {
        let host = cpal::default_host().id();
        let id = DeviceId::new(host, "Mic (USB):1 x");
        let parsed = parse_device_id(&id.to_string()).unwrap();
        assert_eq!(parsed, id);
        assert!(matches!(
            parse_device_id("Built-in Microphone"),
            Err(AudioError::NoDevice(_))
        ));
    }

    #[test]
    fn origin_selection() {
        assert_eq!(best_origin(&[]), (0, false));
        // Preempted between reads (wide bracket) and a big gap: not trusted.
        assert_eq!(best_origin(&[(0, 50_000_000, 5_000_000)]), (0, false));
        // The narrow triple wins over the wide one that would fake an offset.
        let triples = [
            (0, 20_000_000, 9_000_000),
            (100_000_000, 100_000_300, 100_000_600),
            (200_000_000, 150_000_000, 200_900_000),
        ];
        assert_eq!(best_origin(&triples), (0, true));
        // A real separate origin is kept when the bracket is narrow.
        let triples = [(10_000_000, 4_000_000, 10_000_100), (1, 1, 50_000_000)];
        assert_eq!(best_origin(&triples), (6_000_050, true));
    }

    #[test]
    fn latch_keeps_every_event() {
        let latch = Arc::new(EventLatch::new());
        let counts: Arc<Vec<AtomicU32>> =
            Arc::new((0..N_EVENTS).map(|_| AtomicU32::new(0)).collect());
        let c2 = counts.clone();
        let disp = Dispatcher::spawn(
            latch.clone(),
            Box::new(move |ev, _| {
                c2[event_idx(ev)].fetch_add(1, Ordering::Relaxed);
            }),
        );
        let (l1, l2) = (latch.clone(), latch.clone());
        let t1 = std::thread::spawn(move || {
            for i in 0..5000 {
                l1.post(Side::In, BackendEvent::XrunIn);
                if i == 2500 {
                    l1.post(Side::In, BackendEvent::Invalidated);
                }
            }
        });
        let t2 = std::thread::spawn(move || {
            for i in 0..3000 {
                l2.post(Side::Out, BackendEvent::XrunOut);
                if i == 1000 {
                    l2.post(Side::Out, BackendEvent::DeviceLost);
                    l2.post(Side::Out, BackendEvent::Invalidated);
                }
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();
        drop(disp); // final drain, then join
        let get = |e| counts[event_idx(e)].load(Ordering::Relaxed);
        assert_eq!(get(BackendEvent::Invalidated), 2);
        assert_eq!(get(BackendEvent::DeviceLost), 1);
        assert_eq!(get(BackendEvent::XrunIn), 5000);
        assert_eq!(get(BackendEvent::XrunOut), 3000);
    }

    #[test]
    fn latch_delivers_terminal_first() {
        let latch = EventLatch::new();
        latch.post(Side::In, BackendEvent::XrunIn);
        latch.post(Side::Out, BackendEvent::XrunOut);
        latch.post(Side::Out, BackendEvent::DeviceLost);
        latch.post(Side::In, BackendEvent::Invalidated);
        let mut d = Delivered::default();
        let mut log = Vec::new();
        latch.drain(&mut d, &mut |e, _| log.push(e));
        assert_eq!(
            log,
            [
                BackendEvent::Invalidated,
                BackendEvent::DeviceLost,
                BackendEvent::XrunIn,
                BackendEvent::XrunOut
            ]
        );
        log.clear();
        latch.drain(&mut d, &mut |e, _| log.push(e));
        assert!(log.is_empty());
    }

    fn drained(latch: &EventLatch, d: &mut Delivered) -> Vec<(BackendEvent, Option<XrunBoundary>)> {
        let mut log = Vec::new();
        latch.drain(d, &mut |e, at| log.push((e, at)));
        log
    }

    use XrunBoundary::{Exact, Within};

    #[test]
    fn xrun_boundary_is_the_frame_counter_when_the_error_was_raised() {
        use BackendEvent as B;
        let latch = EventLatch::new();
        let mut d = Delivered::default();
        // Before any block: the boundary is frame 0, and it is not "none".
        latch.post(Side::In, B::XrunIn);
        assert_eq!(drained(&latch, &mut d), [(B::XrunIn, Some(Exact(0)))]);
        // The data callback that follows the error advances the counter: the boundary
        // stays where the error saw it.
        latch.set_progress(Side::In, 4800);
        latch.set_progress(Side::Out, 9600);
        latch.post(Side::In, B::XrunIn);
        latch.set_progress(Side::In, 5280);
        latch.set_progress(Side::In, 5760);
        latch.post(Side::Out, B::XrunOut);
        latch.set_progress(Side::Out, 10_080);
        assert_eq!(
            drained(&latch, &mut d),
            [
                (B::XrunIn, Some(Exact(4800))),
                (B::XrunOut, Some(Exact(9600)))
            ]
        );
        // Several xruns of one stream in one dispatch period: each keeps its own
        // boundary, in order (finding 3 of the A1 review: the second one used to come
        // without a boundary and be placed where the engine stood at delivery).
        latch.post(Side::In, B::XrunIn);
        latch.set_progress(Side::In, 6240);
        latch.post(Side::In, B::XrunIn);
        latch.set_progress(Side::In, 6720);
        latch.post(Side::In, B::XrunIn);
        latch.set_progress(Side::In, 7200);
        assert_eq!(
            drained(&latch, &mut d),
            [
                (B::XrunIn, Some(Exact(5760))),
                (B::XrunIn, Some(Exact(6240))),
                (B::XrunIn, Some(Exact(6720)))
            ]
        );
        // Other events never carry a boundary.
        latch.post(Side::In, B::Other);
        latch.post(Side::In, B::DeviceLost);
        latch.post(Side::In, B::XrunIn);
        assert_eq!(
            drained(&latch, &mut d),
            [
                (B::DeviceLost, None),
                (B::Other, None),
                (B::XrunIn, Some(Exact(7200)))
            ]
        );
        assert!(drained(&latch, &mut d).is_empty());
    }

    #[test]
    fn a_full_boundary_ring_is_reported_not_guessed() {
        use BackendEvent as B;
        let latch = EventLatch::new();
        let mut d = Delivered::default();
        latch.set_progress(Side::In, 480);
        latch.post(Side::In, B::XrunIn);
        assert_eq!(drained(&latch, &mut d), [(B::XrunIn, Some(Exact(480)))]);
        // The dispatcher stalls over 64 + 3 xruns of the input, one per block.
        let n = BOUNDARY_SLOTS as u64 + 3;
        for k in 0..n {
            latch.set_progress(Side::In, 960 + 480 * k);
            latch.post(Side::In, B::XrunIn);
        }
        latch.set_progress(Side::In, 960 + 480 * n);
        latch.post(Side::Out, B::XrunOut); // the other stream has its own ring
        let got = drained(&latch, &mut d);
        assert_eq!(got.len() as u64, n + 1, "every xrun is delivered");
        for (k, (ev, at)) in got.iter().take(n as usize).enumerate() {
            assert_eq!(*ev, B::XrunIn);
            let k = k as u64;
            if k < BOUNDARY_SLOTS as u64 {
                assert_eq!(*at, Some(Exact(960 + 480 * k)), "xrun {k}");
            } else {
                // Lost: the span from the latest exact boundary to the counter now. It
                // holds the true boundary; no place inside it is claimed.
                let last = 960 + 480 * (BOUNDARY_SLOTS as u64 - 1);
                let to = 960 + 480 * n;
                assert_eq!(*at, Some(Within { from: last, to }), "xrun {k}");
                assert!((last..=to).contains(&(960 + 480 * k)));
            }
        }
        assert_eq!(got[n as usize], (B::XrunOut, Some(Exact(0))));
        // The ring is free again: exact boundaries from the next xrun on.
        latch.post(Side::In, B::XrunIn);
        assert_eq!(
            drained(&latch, &mut d),
            [(B::XrunIn, Some(Exact(960 + 480 * n)))]
        );
    }

    #[test]
    fn the_data_callbacks_publish_their_frame_counter() {
        let latch = Arc::new(EventLatch::new());
        let mut d = Delivered::default();
        let off = Arc::new(AtomicI64::new(0));
        let mut inp = InputState::<i16>::new(Box::new(|_, _| {}), 2, 48_000, off, latch.clone());
        let mut out =
            OutputState::<f32>::new(Box::new(|b, _| b.fill(0.0)), 2, 48_000, latch.clone());
        inp.scratch.truncate(8); // chunked: the counter is published once, at the end
        inp.block(&[0i16; 20], si(0), si(0)); // 10 frames
        let mut buf = vec![0.0f32; 14]; // 7 frames
        out.block(&mut buf, si(0), si(0));
        // The error is raised before the next block of each stream, as WASAPI capture does.
        latch.post(Side::In, BackendEvent::XrunIn);
        latch.post(Side::Out, BackendEvent::XrunOut);
        inp.block(&[0i16; 6], si(0), si(0));
        out.block(&mut buf, si(0), si(0));
        assert_eq!(
            drained(&latch, &mut d),
            [
                (BackendEvent::XrunIn, Some(Exact(10))),
                (BackendEvent::XrunOut, Some(Exact(7)))
            ]
        );
        // The i16 output path publishes too.
        let latch = Arc::new(EventLatch::new());
        let mut d = Delivered::default();
        let mut out =
            OutputState::<i16>::new(Box::new(|b, _| b.fill(0.0)), 1, 48_000, latch.clone());
        let mut buf = vec![0i16; 5];
        out.block(&mut buf, si(0), si(0));
        latch.post(Side::Out, BackendEvent::XrunOut);
        assert_eq!(
            drained(&latch, &mut d),
            [(BackendEvent::XrunOut, Some(Exact(5)))]
        );
    }

    /// Under a concurrent stream thread every xrun is delivered once, in order, and
    /// each one with *its own* boundary (xrun `k` was raised when the counter was
    /// `k · 480`), or, where the ring was full, with a span that holds it.
    #[test]
    fn boundaries_under_a_concurrent_stream_thread() {
        use std::sync::Mutex;
        let latch = Arc::new(EventLatch::new());
        let got: Arc<Mutex<Vec<Option<XrunBoundary>>>> = Arc::new(Mutex::new(Vec::new()));
        let g2 = got.clone();
        let disp = Dispatcher::spawn(
            latch.clone(),
            Box::new(move |ev, at| {
                assert_eq!(ev, BackendEvent::XrunIn);
                g2.lock().unwrap().push(at);
            }),
        );
        let l1 = latch.clone();
        let n = 20_000u64;
        let stream = std::thread::spawn(move || {
            for k in 0..n {
                // Error first, then the block that follows it, as on the stream thread.
                l1.post(Side::In, BackendEvent::XrunIn);
                l1.set_progress(Side::In, (k + 1) * 480);
                if k % 16 == 0 {
                    std::thread::sleep(Duration::from_micros(50));
                }
            }
        });
        stream.join().unwrap();
        drop(disp);
        let got = got.lock().unwrap();
        assert_eq!(got.len() as u64, n, "every xrun is delivered");
        let (mut exact, mut within) = (0, 0);
        for (k, at) in got.iter().enumerate() {
            let truth = k as u64 * 480;
            match at.expect("a boundary for every xrun") {
                Exact(b) => {
                    assert_eq!(b, truth, "xrun {k}");
                    exact += 1;
                }
                Within { from, to } => {
                    assert!(from <= truth && truth <= to, "xrun {k}: {from}..={to}");
                    within += 1;
                }
            }
        }
        println!("{exact} exact boundaries, {within} spans (ring full)");
        assert!(exact > 0);
    }

    #[test]
    fn f32_blocks_are_chunked() {
        use std::sync::Mutex;
        let log: Arc<Mutex<Vec<(Stamp, usize)>>> = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        let user: InputFn = Box::new(move |d, s| l2.lock().unwrap().push((s, d.len())));
        let mut st = InputState::<f32>::new(
            user,
            2,
            48_000,
            Arc::new(AtomicI64::new(0)),
            Arc::new(EventLatch::new()),
        );
        st.scratch.truncate(8); // 4 frames per chunk
        st.block(&[0.5; 20], si(0), si(0)); // 10 frames
        let l = log.lock().unwrap();
        let frames: Vec<u32> = l.iter().map(|x| x.0.frames).collect();
        assert_eq!(frames, [4, 4, 2]);
        assert_eq!(l[2].0.first, 8);
        assert_eq!(l[1].1, 8);

        // Output: chunks cover the block; stray trailing sample is zeroed.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let user: OutputFn = Box::new(move |b, s| {
            s2.lock().unwrap().push((s.first, s.frames, b.len()));
            b.fill(1.0);
        });
        let mut st = OutputState::<f32>::new(user, 2, 48_000, Arc::new(EventLatch::new()));
        st.scratch.truncate(8);
        let mut d = vec![9.0f32; 21];
        st.block(&mut d, si(0), si(0));
        assert_eq!(*seen.lock().unwrap(), [(0, 4, 8), (4, 4, 8), (8, 2, 4)]);
        assert!(d[..20].iter().all(|&x| x == 1.0));
        assert_eq!(d[20], 0.0);
        assert_eq!(st.frames_done, 10);
    }

    fn si(n: u64) -> StreamInstant {
        StreamInstant::from_nanos(n)
    }

    #[test]
    fn input_chunking_i16() {
        use std::sync::Mutex;
        let log: Arc<Mutex<Vec<(Stamp, usize, f32)>>> = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        let user: InputFn = Box::new(move |d, s| {
            l2.lock().unwrap().push((s, d.len(), d[0]));
        });
        let off = Arc::new(AtomicI64::new(0));
        let mut st = InputState::<i16>::new(user, 2, 48_000, off, Arc::new(EventLatch::new()));
        // Shrink the scratch to force chunking: 4 frames per chunk.
        st.scratch.truncate(8);
        // 10 frames, stereo; the first sample of frame k is k * 1000.
        let data: Vec<i16> = (0..10).flat_map(|k| [k * 1000, 0]).collect();
        st.block(&data, si(1_000_000), si(900_000));
        st.block(&data[..4], si(2_000_000), si(1_900_000));
        let l = log.lock().unwrap();
        assert_eq!(l.len(), 4);
        assert_eq!((l[0].0.first, l[0].0.frames, l[0].1), (0, 4, 8));
        assert_eq!((l[1].0.first, l[1].0.frames), (4, 4));
        assert_eq!((l[2].0.first, l[2].0.frames), (8, 2));
        assert_eq!(l[1].2, i16_to_f32(4000));
        assert_eq!(l[1].0.device_ns, 1_000_000 + 4 * 1_000_000_000 / 48_000);
        assert_eq!(l[2].0.callback_ns, 900_000);
        // Second block continues the running counter.
        assert_eq!((l[3].0.first, l[3].0.frames), (10, 2));
    }

    #[test]
    fn input_offset_applied() {
        use std::sync::Mutex;
        let log: Arc<Mutex<Vec<Stamp>>> = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        let user: InputFn = Box::new(move |_, s| l2.lock().unwrap().push(s));
        let off = Arc::new(AtomicI64::new(5_000));
        let mut st = InputState::<f32>::new(user, 1, 44_100, off, Arc::new(EventLatch::new()));
        st.block(&[0.0; 3], si(100), si(50));
        let l = log.lock().unwrap();
        assert_eq!(
            (l[0].device_ns, l[0].callback_ns, l[0].frames),
            (5_100, 5_050, 3)
        );
    }

    #[test]
    fn output_chunking_and_conversion_i16() {
        let user: OutputFn = Box::new(|buf, s| {
            for (i, x) in buf.iter_mut().enumerate() {
                *x = (s.first as f32 + (i / 2) as f32) / 100.0;
            }
        });
        let mut st = OutputState::<i16>::new(user, 2, 48_000, Arc::new(EventLatch::new()));
        st.scratch.truncate(6); // 3 frames per chunk
        let mut data = vec![7i16; 2 * 7 + 1]; // 7 frames + 1 stray sample
        st.block(&mut data, si(0), si(0));
        for k in 0..7usize {
            let want = f32_to_i16(k as f32 / 100.0);
            assert_eq!(data[2 * k], want, "frame {k}");
            assert_eq!(data[2 * k + 1], want);
        }
        assert_eq!(data[14], 0);
        assert_eq!(st.frames_done, 7);
    }

    #[test]
    fn output_f32_passthrough_counts_frames() {
        let seen = Arc::new(AtomicU64::new(0));
        let s2 = seen.clone();
        let user: OutputFn = Box::new(move |buf, s| {
            s2.store(s.first, Ordering::Relaxed);
            buf.fill(0.25);
        });
        let mut st = OutputState::<f32>::new(user, 2, 48_000, Arc::new(EventLatch::new()));
        let mut d = vec![0.0f32; 20];
        st.block(&mut d, si(10), si(5));
        st.block(&mut d, si(20), si(15));
        assert_eq!(seen.load(Ordering::Relaxed), 10);
        assert!(d.iter().all(|&x| x == 0.25));
    }

    /// Opens the default devices for 2 s. Needs hardware and the macOS
    /// microphone permission, so it is ignored by default.
    #[test]
    #[ignore]
    fn opens_default_devices() {
        let blocks_in = Arc::new(AtomicU32::new(0));
        let blocks_out = Arc::new(AtomicU32::new(0));
        let bad = Arc::new(AtomicU32::new(0));
        let (last_in, last_out) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (bi, bo, b1, b2) = (
            blocks_in.clone(),
            blocks_out.clone(),
            bad.clone(),
            bad.clone(),
        );
        let (li, lo) = (last_in.clone(), last_out.clone());
        let io = IoCallbacks {
            input: Box::new(move |d, s| {
                let end = li.load(Ordering::Relaxed);
                if bi.fetch_add(1, Ordering::Relaxed) > 0 && s.first != end {
                    b1.fetch_add(1, Ordering::Relaxed);
                }
                assert!(!d.is_empty());
                li.store(s.first + u64::from(s.frames), Ordering::Relaxed);
            }),
            output: Box::new(move |d, s| {
                let end = lo.load(Ordering::Relaxed);
                if bo.fetch_add(1, Ordering::Relaxed) > 0 && s.first != end {
                    b2.fetch_add(1, Ordering::Relaxed);
                }
                d.fill(0.0);
                lo.store(s.first + u64::from(s.frames), Ordering::Relaxed);
            }),
            error: Box::new(|_, _| {}),
        };
        let (streams, info) = CpalBackend::new()
            .open(&OpenRequest::default(), io)
            .expect("open default devices");
        assert!(info.fs_in == 44_100 || info.fs_in == 48_000);
        let t0 = streams.host_now_ns();
        std::thread::sleep(Duration::from_secs(2));
        let dt = streams.host_now_ns() - t0;
        assert!(
            (1_900_000_000..2_500_000_000).contains(&dt),
            "host clock: {dt} ns"
        );
        drop(streams);
        assert!(blocks_in.load(Ordering::Relaxed) > 10);
        assert!(blocks_out.load(Ordering::Relaxed) > 10);
        assert_eq!(bad.load(Ordering::Relaxed), 0, "stamps must be contiguous");
        assert!(last_in.load(Ordering::Relaxed) > u64::from(info.fs_in) / 2);
    }
}
