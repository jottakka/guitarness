//! Fake audio backend on a virtual host clock: the test oracle of the crate.
//!
//! [`FakeBackend`] implements [`Backend`] without a sound card, a thread or real time.
//! A test keeps a [`FakeHandle`], hands the backend to the code under test and advances
//! virtual time with [`FakeHandle::pump`]; every callback that falls due runs on the
//! calling thread, in time order. The same seed gives the same stamps and samples, however
//! the time is sliced between `pump` calls.
//!
//! # The model
//!
//! True time `t` is in seconds since the backend was created. The host clock reads
//! `host_origin_ns + t`.
//!
//! - **Output device.** Runs at `fs_out·(1 + drift_out)` frames per second. Device
//!   position `P` leaves the loudspeaker at `t_out0 + P/rate_out`. The callback for a
//!   block runs `out_lead_s` before its first frame plays.
//! - **Input device.** Runs at `fs_in·(1 + drift_in)`. Device position `Q` is captured at
//!   `t_in0 + Q/rate_in`. The callback for a block runs `in_lead_s` after its last frame.
//! - **Stamps.** `device_ns` is the true play or capture time of the first frame of the
//!   block, plus Gaussian jitter (and an optional rate skew or one-off outlier).
//!   `callback_ns` is the time of the callback. The input device clock reads
//!   [`FakeConfig::origin_offset_ns`] behind the host clock. Like the cpal backend, the
//!   fake adds that offset to the input stamps itself: the stamps of both streams arrive
//!   on the host clock and [`StreamInfo::origin_offset_ns`] is only a report. A client
//!   must not add it again. [`FakeConfig::raw_input_stamps`] delivers the raw input clock
//!   instead, for a client that applies the offset (`ClockConfig::origin_offset_ns`).
//! - **Acoustic path.** The microphone hears the loudspeaker `latency_s` later:
//!   `mic(t) = gain · (ir ∗ speaker)(t − latency_s) + noise + external(t)`. The speaker
//!   signal is the band-limited reconstruction of the mean of the output channels (a
//!   64-tap Kaiser-windowed sinc, 80 dB), read at the exact fractional position, so the
//!   delay is right to a small fraction of a sample for any pair of rates and drifts.
//! - **Xruns.** [`FakeHandle::inject_xrun`] makes the device run on for some frames at
//!   the next block boundary: input frames are captured but never delivered, output
//!   frames play as silence. The frame counters of the callbacks stay contiguous, the
//!   stamps jump, and the error callback gets `XrunIn` / `XrunOut` with the boundary: the
//!   frame counter of that stream at the xrun, which is the `first` of the next block
//!   ([`FakeConfig::error_boundary`] off: no boundary, like a backend that cannot tell).
//!   The notice is raised just before the callback of that block, as WASAPI capture does.
//!   [`FakeConfig::error_delay_s`] delivers it that much later instead, after the block
//!   has run: the dispatcher thread of the cpal backend.
//! - **Device events.** [`FakeHandle::inject_event`] calls the error callback.
//!   `Invalidated` and `DeviceLost` also stop both streams for good.
//!
//! [`FakeTruth`] gives the exact relation between frames and time, for assertions.
//!
//! # Real-time rules
//!
//! The callbacks receive slices of buffers allocated in `open`. `pump` itself does not
//! allocate either. The callbacks run while the backend is locked: they must not call
//! the [`FakeHandle`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use guitarness_dsp::synth::Mulberry32;

use crate::backend::{Backend, HostClock, IoCallbacks, XrunBoundary};
use crate::types::{AudioError, BackendEvent, DeviceInfo, OpenRequest, Side, Stamp, StreamInfo};

/// Id of the fake input device.
pub const FAKE_INPUT_ID: &str = "fake-in";
/// Id of the fake output device.
pub const FAKE_OUTPUT_ID: &str = "fake-out";

/// Half width of the interpolation kernel, in output frames.
const HALF: usize = 32;
const TAPS: usize = 2 * HALF;
const PHASES: usize = 256;
/// Stop-band attenuation of the kernel, dB.
const KERNEL_DB: f64 = 80.0;
const MAX_VOICES: usize = 64;
const MAX_GAPS: usize = 1024;
const MAX_EVENTS: usize = 16;
/// Xrun notices waiting for their delayed delivery.
const MAX_LATE: usize = 64;
const MAX_BLOCK: u32 = 1 << 16;

/// Sizes of the callback blocks of one stream, in frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockModel {
    /// Always the same size.
    Fixed(u32),
    /// Uniform in `min..=max`, drawn per block.
    Uniform {
        /// Smallest block.
        min: u32,
        /// Largest block.
        max: u32,
    },
    /// The listed sizes, repeated.
    Cycle(Vec<u32>),
}

impl BlockModel {
    fn bounds(&self) -> Option<(u32, u32)> {
        match self {
            Self::Fixed(n) => Some((*n, *n)),
            Self::Uniform { min, max } if min <= max => Some((*min, *max)),
            Self::Uniform { .. } => None,
            Self::Cycle(v) => Some((*v.iter().min()?, *v.iter().max()?)),
        }
    }
}

/// The virtual acoustic path from the output to the input.
#[derive(Clone, Debug, PartialEq)]
pub struct FakePath {
    /// Delay from the loudspeaker to the microphone, seconds. With exact stamps this is
    /// the round-trip latency `L_rt` a calibration must find.
    pub latency_s: f64,
    /// Linear gain of the path. 0 disconnects it.
    pub gain: f32,
    /// RMS of white Gaussian noise added at the microphone.
    pub noise_rms: f32,
    /// Impulse response at the input rate, applied after the delay: tap `j` adds
    /// `j / fs_in` seconds. `None` is a single unit tap.
    pub ir: Option<Vec<f32>>,
    /// Largest latency [`FakeHandle::set_latency_s`] may set, seconds (sizes the history).
    pub max_latency_s: f64,
}

impl Default for FakePath {
    fn default() -> Self {
        Self {
            latency_s: 0.035,
            gain: 0.5,
            noise_rms: 0.0,
            ir: None,
            max_latency_s: 1.0,
        }
    }
}

/// Everything the fake devices do. `Default` is a quiet 48 kHz pair with 10 ms blocks.
#[derive(Clone, Debug, PartialEq)]
pub struct FakeConfig {
    /// Nominal input rate, Hz (44100 or 48000).
    pub fs_in: u32,
    /// Nominal output rate, Hz (44100 or 48000).
    pub fs_out: u32,
    /// Input channels; the microphone signal is copied to all of them.
    pub ch_in: u16,
    /// Output channels; the loudspeaker plays their mean.
    pub ch_out: u16,
    /// Error of the input device clock, ppm (positive: runs fast).
    pub drift_in_ppm: f64,
    /// Error of the output device clock, ppm.
    pub drift_out_ppm: f64,
    /// Block sizes of the input callbacks ([`OpenRequest::buffer_frames`] overrides).
    pub blocks_in: BlockModel,
    /// Block sizes of the output callbacks ([`OpenRequest::buffer_frames`] overrides).
    pub blocks_out: BlockModel,
    /// Standard deviation of the jitter of the input `device_ns`, seconds.
    pub stamp_jitter_in_s: f64,
    /// Standard deviation of the jitter of the output `device_ns`, seconds.
    pub stamp_jitter_out_s: f64,
    /// Rate error of the input `device_ns` themselves, ppm (bad stamps).
    pub stamp_skew_in_ppm: f64,
    /// Rate error of the output `device_ns` themselves, ppm (bad stamps).
    pub stamp_skew_out_ppm: f64,
    /// Scale of the scheduling delay of every callback, seconds (half-normal).
    pub callback_jitter_s: f64,
    /// How long before its first frame plays an output block is requested, seconds.
    pub out_lead_s: f64,
    /// How long after its last frame was captured an input block is delivered, seconds.
    pub in_lead_s: f64,
    /// Capture time of input frame 0, seconds after `open`.
    pub in_start_s: f64,
    /// Host clock reading at true time zero, ns.
    pub host_origin_ns: u64,
    /// The input device clock reads this far behind the host clock, ns. Reported in
    /// [`StreamInfo::origin_offset_ns`]; already added to the input stamps unless
    /// [`FakeConfig::raw_input_stamps`] is set.
    pub origin_offset_ns: i64,
    /// Deliver the input stamps on the raw input clock (the client adds the offset).
    /// Default `false`: the stamps arrive on the host clock, as from the cpal backend.
    pub raw_input_stamps: bool,
    /// The acoustic path.
    pub path: FakePath,
    /// Report injected output xruns to the error callback (WASAPI does not).
    pub notify_out_xrun: bool,
    /// Delay of the xrun notices, seconds. 0: the error callback runs just before the
    /// callback of the first block after the xrun. Above 0: it runs that much later, like
    /// the dispatcher thread of the cpal backend (the block has run by then). Events of
    /// [`FakeHandle::inject_event`] are not delayed.
    pub error_delay_s: f64,
    /// Hand the xrun boundary to the error callback (default). `false`: `None`, as from a
    /// backend that does not know where the stream was when the xrun happened.
    pub error_boundary: bool,
    /// Fill the output buffer with NaN before each callback: a mixer that does not write
    /// every sample shows up in [`FakeStats::nonfinite_out`]. Otherwise it is zeroed.
    pub poison_out: bool,
    /// Clip the converters to ±1.
    pub clip: bool,
    /// Seed of every random process.
    pub seed: u32,
}

impl Default for FakeConfig {
    fn default() -> Self {
        Self {
            fs_in: 48_000,
            fs_out: 48_000,
            ch_in: 1,
            ch_out: 2,
            drift_in_ppm: 0.0,
            drift_out_ppm: 0.0,
            blocks_in: BlockModel::Fixed(480),
            blocks_out: BlockModel::Fixed(480),
            stamp_jitter_in_s: 20.0e-6,
            stamp_jitter_out_s: 20.0e-6,
            stamp_skew_in_ppm: 0.0,
            stamp_skew_out_ppm: 0.0,
            callback_jitter_s: 200.0e-6,
            out_lead_s: 0.020,
            in_lead_s: 0.002,
            in_start_s: 0.0037,
            host_origin_ns: 3_600_000_000_000,
            origin_offset_ns: 0,
            raw_input_stamps: false,
            path: FakePath::default(),
            notify_out_xrun: true,
            error_delay_s: 0.0,
            error_boundary: true,
            poison_out: true,
            clip: true,
            seed: 1,
        }
    }
}

impl FakeConfig {
    /// Defaults at the given nominal rates.
    pub fn new(fs_in: u32, fs_out: u32) -> Self {
        Self {
            fs_in,
            fs_out,
            ..Self::default()
        }
    }

    fn validate(&self) -> Result<(), AudioError> {
        let bad = |what: &str| Err(AudioError::Backend(format!("fake config: {what}")));
        for fs in [self.fs_in, self.fs_out] {
            if fs != 44_100 && fs != 48_000 {
                return Err(AudioError::UnsupportedRate(fs));
            }
        }
        if self.ch_in == 0 || self.ch_out == 0 {
            return bad("zero channels");
        }
        for blocks in [&self.blocks_in, &self.blocks_out] {
            match blocks.bounds() {
                Some((min, max)) if min >= 1 && max <= MAX_BLOCK => {}
                _ => return bad("block sizes must be in 1..=65536"),
            }
        }
        let times = [
            self.stamp_jitter_in_s,
            self.stamp_jitter_out_s,
            self.callback_jitter_s,
            self.in_lead_s,
            self.in_start_s,
            self.path.latency_s,
            self.error_delay_s,
        ];
        if times.iter().any(|t| !t.is_finite() || *t < 0.0) {
            return bad("times must be finite and not negative");
        }
        if !self.out_lead_s.is_finite() || self.out_lead_s < 1.0e-3 {
            return bad("out_lead_s must be at least 1 ms");
        }
        if !self.path.max_latency_s.is_finite() || self.path.latency_s > self.path.max_latency_s {
            return bad("latency_s above max_latency_s");
        }
        let rates = [
            self.drift_in_ppm,
            self.drift_out_ppm,
            self.stamp_skew_in_ppm,
            self.stamp_skew_out_ppm,
        ];
        if rates.iter().any(|r| !r.is_finite() || r.abs() > 2.0e5) {
            return bad("drift and skew must be within ±200000 ppm");
        }
        if let Some(ir) = &self.path.ir
            && (ir.is_empty() || ir.iter().any(|c| !c.is_finite()))
        {
            return bad("impulse response must be finite and not empty");
        }
        Ok(())
    }
}

/// External sound source: adds to the mono microphone block that starts at the given
/// input device position (lost frames included in the count).
pub type ExternalFn = Box<dyn FnMut(u64, &mut [f32]) + Send>;

/// Counters of the fake, cumulative since it was created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FakeStats {
    /// Successful `open` calls.
    pub opens: u64,
    /// Input callbacks run.
    pub callbacks_in: u64,
    /// Output callbacks run.
    pub callbacks_out: u64,
    /// Frames delivered to the input callback.
    pub frames_in: u64,
    /// Frames requested from the output callback.
    pub frames_out: u64,
    /// Input xruns injected.
    pub xruns_in: u64,
    /// Output xruns injected.
    pub xruns_out: u64,
    /// Events delivered to the error callback (xruns included).
    pub events: u64,
    /// Injected events discarded because the queue was full. `Invalidated` and
    /// `DeviceLost` are never discarded.
    pub events_dropped: u64,
    /// Output frames the callback left NaN or infinite (played as silence).
    pub nonfinite_out: u64,
    /// Microphone samples that needed output not rendered yet (a causality error of the
    /// configuration: latency plus leads too short).
    pub path_starved: u64,
    /// Microphone samples that needed output older than the history.
    pub path_overrun: u64,
}

// ------------------------------------------------------------------ random numbers

struct Gauss {
    rng: Mulberry32,
    spare: Option<f64>,
}

impl Gauss {
    fn new(seed: u32) -> Self {
        Self {
            rng: Mulberry32::new(seed),
            spare: None,
        }
    }

    fn normal(&mut self) -> f64 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let u1 = 1.0 - self.rng.next_f64(); // (0, 1]
        let u2 = self.rng.next_f64();
        let radius = (-2.0 * u1.ln()).sqrt();
        let (s, c) = (2.0 * std::f64::consts::PI * u2).sin_cos();
        self.spare = Some(radius * s);
        radius * c
    }
}

/// Independent seed for process `k` of session `session`.
fn sub_seed(seed: u32, session: u64, k: u32) -> u32 {
    let mut x = seed ^ k.wrapping_mul(0x9E37_79B9) ^ (session as u32).wrapping_mul(0x7FEB_352D);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    x = x.wrapping_mul(0xC2B2_AE35);
    x ^ (x >> 16)
}

struct BlockGen {
    model: BlockModel,
    rng: Mulberry32,
    idx: usize,
}

impl BlockGen {
    fn next(&mut self) -> u32 {
        match &self.model {
            BlockModel::Fixed(n) => *n,
            BlockModel::Uniform { min, max } => {
                let span = f64::from(max - min + 1);
                (min + (self.rng.next_f64() * span) as u32).min(*max)
            }
            BlockModel::Cycle(v) => {
                let n = v[self.idx % v.len()];
                self.idx += 1;
                n
            }
        }
    }
}

// ------------------------------------------------------------------ timelines

/// From callback frame `frame` on, the device position is `frame + total`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gap {
    frame: u64,
    total: u64,
}

/// Device position ↔ true time ↔ callback frame of one stream.
#[derive(Clone, Debug, PartialEq)]
struct Timeline {
    /// True frames per second.
    rate: f64,
    /// True time of device position 0.
    t0: f64,
    /// Lost frames before the first recorded gap (older gaps are forgotten).
    base: u64,
    gaps: Vec<Gap>,
}

impl Timeline {
    fn total(&self) -> u64 {
        self.gaps.last().map_or(self.base, |g| g.total)
    }

    fn pos_of_frame(&self, frame: f64) -> f64 {
        let mut total = self.base;
        for g in &self.gaps {
            if (g.frame as f64) <= frame {
                total = g.total;
            } else {
                break;
            }
        }
        frame + total as f64
    }

    /// Callback frame at device position `pos`; a position inside a hole maps to the
    /// first frame after it.
    fn frame_of_pos(&self, pos: f64) -> f64 {
        let mut before = self.base;
        for g in &self.gaps {
            if pos < (g.frame + before) as f64 {
                break;
            }
            if pos < (g.frame + g.total) as f64 {
                return g.frame as f64;
            }
            before = g.total;
        }
        pos - before as f64
    }

    fn time_of_frame(&self, frame: f64) -> f64 {
        self.t0 + self.pos_of_frame(frame) / self.rate
    }

    fn frame_at(&self, t: f64) -> f64 {
        self.frame_of_pos((t - self.t0) * self.rate)
    }

    fn push_gap(&mut self, frame: u64, lost: u64) {
        let total = self.total() + lost;
        if self.gaps.len() == MAX_GAPS {
            self.base = self.gaps.remove(0).total;
        }
        self.gaps.push(Gap { frame, total });
    }
}

/// The exact relation between frames and true time of the open streams, for assertions.
/// A copy: it does not follow xruns injected later.
#[derive(Clone, Debug, PartialEq)]
pub struct FakeTruth {
    /// Nominal input rate, Hz.
    pub fs_in: f64,
    /// Nominal output rate, Hz.
    pub fs_out: f64,
    /// Latency of the acoustic path when the copy was taken, seconds.
    pub latency_s: f64,
    inp: Timeline,
    out: Timeline,
}

impl FakeTruth {
    /// True input frames per second.
    pub fn rate_in(&self) -> f64 {
        self.inp.rate
    }

    /// True output frames per second.
    pub fn rate_out(&self) -> f64 {
        self.out.rate
    }

    /// True time at which output frame `f` leaves the loudspeaker, seconds.
    pub fn out_time_s(&self, f: f64) -> f64 {
        self.out.time_of_frame(f)
    }

    /// True time at which input frame `g` is captured, seconds.
    pub fn in_time_s(&self, g: f64) -> f64 {
        self.inp.time_of_frame(g)
    }

    /// Output frame playing at true time `t`.
    pub fn out_frame_at(&self, t: f64) -> f64 {
        self.out.frame_at(t)
    }

    /// Input frame captured at true time `t`.
    pub fn in_frame_at(&self, t: f64) -> f64 {
        self.inp.frame_at(t)
    }

    /// Input frame captured at the instant output frame `f` plays: what
    /// `ClockSnapshot::in_of_out` estimates.
    pub fn in_of_out(&self, f: f64) -> f64 {
        self.in_frame_at(self.out_time_s(f))
    }

    /// Output frame playing at the instant input frame `g` is captured.
    pub fn out_of_in(&self, g: f64) -> f64 {
        self.out_frame_at(self.in_time_s(g))
    }

    /// Input frame at which output frame `f` arrives through the acoustic path (tap 0 of
    /// the impulse response).
    pub fn arrival_in(&self, f: f64) -> f64 {
        self.in_frame_at(self.out_time_s(f) + self.latency_s)
    }

    /// Input clock relative to the output clock, ppm: the true value of
    /// `ClockSnapshot::drift_ppm`.
    pub fn drift_ppm(&self) -> f64 {
        ((self.inp.rate / self.fs_in) / (self.out.rate / self.fs_out) - 1.0) * 1.0e6
    }
}

// ------------------------------------------------------------------ streams

struct Stream {
    side: Side,
    ch: usize,
    line: Timeline,
    /// True time of `open`.
    t_open: f64,
    /// Device position of the next block.
    pos: u64,
    /// Callback frame of the next block.
    frame: u64,
    /// Size of the next block.
    frames: u32,
    /// Scheduling delay drawn for the next block.
    late_s: f64,
    /// True time of the next callback.
    cb_time: f64,
    last_cb_time: f64,
    lead_s: f64,
    blocks: BlockGen,
    rng_stamp: Gauss,
    rng_late: Gauss,
    stamp_jitter_s: f64,
    stamp_skew: f64,
    late_scale_s: f64,
    outlier_s: f64,
    notify_xrun: bool,
}

impl Stream {
    fn device_time(&self) -> f64 {
        self.line.t0 + self.pos as f64 / self.line.rate
    }

    fn retime(&mut self) {
        let t = match self.side {
            Side::Out => self.device_time() - self.lead_s + self.late_s,
            Side::In => {
                let end = self.pos + u64::from(self.frames);
                self.line.t0 + end as f64 / self.line.rate + self.lead_s + self.late_s
            }
        };
        self.cb_time = t.max(self.last_cb_time);
    }

    fn schedule(&mut self) {
        self.frames = self.blocks.next();
        let late = self.rng_late.normal().abs() * self.late_scale_s;
        self.late_s = match self.side {
            // The data must be there before it plays.
            Side::Out => late.min(0.5 * self.lead_s),
            Side::In => late,
        };
        self.retime();
    }

    fn stamp(&mut self, host_origin_ns: u64, shift_ns: i64) -> Stamp {
        let t = self.device_time();
        let apparent = t
            + self.stamp_skew * (t - self.t_open)
            + self.rng_stamp.normal() * self.stamp_jitter_s
            + std::mem::take(&mut self.outlier_s);
        let ns = |t: f64| {
            let v = i128::from(host_origin_ns) + (t * 1.0e9).round() as i128 - i128::from(shift_ns);
            v.max(0) as u64
        };
        Stamp {
            first: self.frame,
            frames: self.frames,
            device_ns: ns(apparent),
            callback_ns: ns(self.cb_time),
        }
    }

    fn advance(&mut self) {
        self.pos += u64::from(self.frames);
        self.frame += u64::from(self.frames);
        self.last_cb_time = self.cb_time;
        self.schedule();
    }

    /// The device runs `lost` frames before the next block.
    fn add_gap(&mut self, lost: u64) {
        self.line.push_gap(self.frame, lost);
        self.pos += lost;
        self.retime();
    }
}

/// History of the loudspeaker signal and the interpolation kernel.
struct Speaker {
    ring: Vec<f32>,
    mask: usize,
    /// Positions below `end` are rendered.
    end: u64,
    /// One past the last rendered non-zero sample.
    nz_end: u64,
    /// `(PHASES + 1) × TAPS` kernel rows.
    table: Vec<f32>,
}

impl Speaker {
    fn new(frames: usize, ratio_in_out: f64) -> Self {
        let len = frames.next_power_of_two();
        Self {
            ring: vec![0.0; len],
            mask: len - 1,
            end: 0,
            nz_end: 0,
            table: kernel_table(ratio_in_out),
        }
    }

    /// The speaker signal at fractional device position `phi`.
    #[inline]
    fn sample(&self, phi: f64, stats: &mut FakeStats) -> f32 {
        let floor = phi.floor();
        let frac = phi - floor;
        let ip = floor as i64;
        let lo = ip - HALF as i64 + 1;
        let hi = ip + HALF as i64;
        let end = self.end as i64;
        if hi < 0 {
            return 0.0; // before the stream started
        }
        if hi >= end {
            stats.path_starved += 1;
        }
        if lo >= self.nz_end as i64 {
            return 0.0; // silence since
        }
        if lo < end - self.ring.len() as i64 {
            stats.path_overrun += 1;
            return 0.0;
        }
        let ph = frac * PHASES as f64;
        let k = (ph as usize).min(PHASES - 1);
        let a = (ph - k as f64) as f32;
        let row0 = &self.table[k * TAPS..(k + 1) * TAPS];
        let row1 = &self.table[(k + 1) * TAPS..(k + 2) * TAPS];
        let j0 = (-lo).max(0) as usize;
        let j1 = (end - lo).clamp(0, TAPS as i64) as usize;
        let mut acc = 0.0f32;
        for j in j0..j1 {
            let v = self.ring[(lo + j as i64) as usize & self.mask];
            acc += v * (row0[j] + a * (row1[j] - row0[j]));
        }
        acc
    }

    fn write(&mut self, pos: u64, v: f32) {
        self.ring[pos as usize & self.mask] = v;
        if v != 0.0 {
            self.nz_end = pos + 1;
        }
    }

    fn silence(&mut self, from: u64, frames: u64) {
        if frames >= self.ring.len() as u64 {
            self.ring.fill(0.0);
        } else {
            for p in from..from + frames {
                self.ring[p as usize & self.mask] = 0.0;
            }
        }
    }
}

/// Modified Bessel function I0.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1.0e-17 * sum {
        term *= (x / (2.0 * k)) * (x / (2.0 * k));
        sum += term;
        k += 1.0;
    }
    sum
}

/// Kaiser-windowed sinc, tabulated for `PHASES + 1` fractional positions. Row `k`, tap
/// `j` weighs the sample `j − (HALF − 1) − k/PHASES` frames from the read position.
/// The cut-off leaves the stop band above the Nyquist frequency of the slower stream;
/// every row sums to one, so the gain at DC is exact at every position.
fn kernel_table(ratio_in_out: f64) -> Vec<f32> {
    let beta = 0.1102 * (KERNEL_DB - 8.7);
    let transition = (KERNEL_DB - 7.95) / (14.36 * (TAPS as f64 - 1.0));
    let fc = 0.5 * ratio_in_out.min(1.0) - 0.5 * transition;
    let i0_beta = bessel_i0(beta);
    let mut table = Vec::with_capacity((PHASES + 1) * TAPS);
    let mut row = [0.0f64; TAPS];
    for k in 0..=PHASES {
        let frac = k as f64 / PHASES as f64;
        let mut sum = 0.0;
        for (j, r) in row.iter_mut().enumerate() {
            let x = j as f64 - (HALF as f64 - 1.0) - frac;
            let u = x / HALF as f64;
            let window = if u.abs() < 1.0 {
                bessel_i0(beta * (1.0 - u * u).sqrt()) / i0_beta
            } else {
                0.0
            };
            let arg = std::f64::consts::PI * 2.0 * fc * x;
            let sinc = if arg.abs() < 1.0e-9 {
                1.0
            } else {
                arg.sin() / arg
            };
            *r = 2.0 * fc * sinc * window;
            sum += *r;
        }
        table.extend(row.iter().map(|r| (r / sum) as f32));
    }
    table
}

struct Voice {
    /// Input device position of the first sample.
    start: u64,
    samples: Arc<[f32]>,
    gain: f32,
}

struct Session {
    id: u64,
    io: IoCallbacks,
    dead: bool,
    out: Stream,
    inp: Stream,
    out_buf: Vec<f32>,
    in_buf: Vec<f32>,
    mic: Vec<f32>,
    speaker: Speaker,
    /// No impulse response, or a unit one: the microphone hears the path signal itself.
    direct: bool,
    /// Non-zero taps of the impulse response (not used when `direct`). None: silence.
    ir: Vec<(u64, f32)>,
    ir_len: u64,
    /// Path signal before the impulse response, by input device position.
    xring: Vec<f32>,
    xmask: usize,
    x_end: u64,
    noise: Gauss,
    /// Xrun notices not delivered yet, oldest first: `(due time, event, boundary)`.
    late: [(f64, BackendEvent, Option<XrunBoundary>); MAX_LATE],
    n_late: usize,
}

impl Session {
    fn new(cfg: &FakeConfig, req: &OpenRequest, io: IoCallbacks, now: f64, id: u64) -> Self {
        let pick = |model: &BlockModel| match req.buffer_frames {
            Some(n) => BlockModel::Fixed(n.clamp(1, MAX_BLOCK)),
            None => model.clone(),
        };
        let stream = |side: Side, k: u32| {
            let (fs, ch, drift, model, jitter, skew, lead, t0) = match side {
                Side::In => (
                    cfg.fs_in,
                    cfg.ch_in,
                    cfg.drift_in_ppm,
                    pick(&cfg.blocks_in),
                    cfg.stamp_jitter_in_s,
                    cfg.stamp_skew_in_ppm,
                    cfg.in_lead_s,
                    now + cfg.in_start_s,
                ),
                Side::Out => (
                    cfg.fs_out,
                    cfg.ch_out,
                    cfg.drift_out_ppm,
                    pick(&cfg.blocks_out),
                    cfg.stamp_jitter_out_s,
                    cfg.stamp_skew_out_ppm,
                    cfg.out_lead_s,
                    now + cfg.out_lead_s,
                ),
            };
            let mut s = Stream {
                side,
                ch: usize::from(ch),
                line: Timeline {
                    rate: f64::from(fs) * (1.0 + drift * 1.0e-6),
                    t0,
                    base: 0,
                    gaps: Vec::with_capacity(MAX_GAPS),
                },
                t_open: now,
                pos: 0,
                frame: 0,
                frames: 0,
                late_s: 0.0,
                cb_time: now,
                last_cb_time: now,
                lead_s: lead,
                blocks: BlockGen {
                    model,
                    rng: Mulberry32::new(sub_seed(cfg.seed, id, k)),
                    idx: 0,
                },
                rng_stamp: Gauss::new(sub_seed(cfg.seed, id, k + 1)),
                rng_late: Gauss::new(sub_seed(cfg.seed, id, k + 2)),
                stamp_jitter_s: jitter,
                stamp_skew: skew * 1.0e-6,
                late_scale_s: cfg.callback_jitter_s,
                outlier_s: 0.0,
                notify_xrun: false,
            };
            s.schedule();
            s
        };
        let inp = stream(Side::In, 10);
        let out = stream(Side::Out, 20);
        let max_in = pick(&cfg.blocks_in).bounds().map_or(1, |b| b.1) as usize;
        let max_out = pick(&cfg.blocks_out).bounds().map_or(1, |b| b.1) as usize;
        let (fs_in, fs_out) = (f64::from(cfg.fs_in), f64::from(cfg.fs_out));

        let ir: Vec<(u64, f32)> = cfg.path.ir.as_ref().map_or_else(Vec::new, |ir| {
            ir.iter()
                .enumerate()
                .filter(|(_, c)| **c != 0.0)
                .map(|(j, c)| (j as u64, *c))
                .collect()
        });
        let ir_len = cfg.path.ir.as_ref().map_or(0, Vec::len);
        let unit = ir_len == 0 || (ir.len() == 1 && ir[0] == (0, 1.0));
        let xlen = if unit {
            1
        } else {
            (ir_len + max_in).next_power_of_two()
        };

        // History: the longest look-back of the path, plus everything already rendered
        // ahead of the capture instant, plus slack.
        let history_s = cfg.path.max_latency_s
            + ir_len as f64 / fs_in
            + cfg.out_lead_s
            + cfg.in_lead_s
            + 2.0 * (max_in as f64 / fs_in + max_out as f64 / fs_out)
            + 10.0 * cfg.callback_jitter_s
            + 0.25;
        let history = (history_s * fs_out * 1.01) as usize + 4 * TAPS;

        Self {
            id,
            io,
            dead: false,
            out_buf: vec![0.0; max_out * usize::from(cfg.ch_out)],
            in_buf: vec![0.0; max_in * usize::from(cfg.ch_in)],
            mic: vec![0.0; max_in],
            speaker: Speaker::new(history, fs_in / fs_out),
            direct: unit,
            ir: if unit { Vec::new() } else { ir },
            ir_len: if unit { 0 } else { ir_len as u64 },
            xring: vec![0.0; xlen],
            xmask: xlen - 1,
            x_end: 0,
            noise: Gauss::new(sub_seed(cfg.seed, id, 30)),
            late: [(0.0, BackendEvent::Other, None); MAX_LATE],
            n_late: 0,
            inp,
            out,
        }
    }

    /// The xrun notice of the block of `side` that is about to run: delivered now, or
    /// queued for `error_delay_s` later.
    fn notify_xrun(&mut self, cfg: &FakeConfig, side: Side, stats: &mut FakeStats) {
        let (ev, frame, at) = match side {
            Side::In => (BackendEvent::XrunIn, self.inp.frame, self.inp.cb_time),
            Side::Out => (BackendEvent::XrunOut, self.out.frame, self.out.cb_time),
        };
        let boundary = cfg.error_boundary.then_some(XrunBoundary::Exact(frame));
        if cfg.error_delay_s > 0.0 {
            if self.n_late == MAX_LATE {
                // Never lost: the oldest one goes out early.
                self.deliver_late(f64::INFINITY, 1, stats);
            }
            self.late[self.n_late] = (at + cfg.error_delay_s, ev, boundary);
            self.n_late += 1;
        } else {
            (self.io.error)(ev, boundary);
            stats.events += 1;
        }
    }

    /// Delivers up to `max` queued xrun notices that are due by `by`. Returns the due time
    /// of the last one delivered.
    fn deliver_late(&mut self, by: f64, max: usize, stats: &mut FakeStats) -> Option<f64> {
        let mut n = 0;
        while n < self.n_late.min(max) && self.late[n].0 <= by {
            let (_, ev, boundary) = self.late[n];
            (self.io.error)(ev, boundary);
            stats.events += 1;
            n += 1;
        }
        if n == 0 {
            return None;
        }
        let last = self.late[n - 1].0;
        self.late.copy_within(n..self.n_late, 0);
        self.n_late -= n;
        Some(last)
    }

    fn next(&self) -> (Side, f64) {
        // Ties: output first.
        if self.out.cb_time <= self.inp.cb_time {
            (Side::Out, self.out.cb_time)
        } else {
            (Side::In, self.inp.cb_time)
        }
    }

    fn run_out(&mut self, cfg: &FakeConfig, stats: &mut FakeStats) {
        let frames = self.out.frames as usize;
        let ch = self.out.ch;
        let stamp = self.out.stamp(cfg.host_origin_ns, 0);
        if std::mem::take(&mut self.out.notify_xrun) {
            self.notify_xrun(cfg, Side::Out, stats);
        }
        let buf = &mut self.out_buf[..frames * ch];
        buf.fill(if cfg.poison_out { f32::NAN } else { 0.0 });
        (self.io.output)(buf, stamp);

        let pos = self.out.pos;
        let scale = 1.0 / ch as f32;
        for (i, frame) in buf.chunks_exact(ch).enumerate() {
            // Checked before the converter clips: clipping would turn ±∞ into full scale.
            let mut sum = 0.0f32;
            let mut finite = true;
            for v in frame {
                finite &= v.is_finite();
                sum += if cfg.clip { v.clamp(-1.0, 1.0) } else { *v };
            }
            let mut mono = sum * scale;
            if !finite || !mono.is_finite() {
                stats.nonfinite_out += 1;
                mono = 0.0;
            }
            self.speaker.write(pos + i as u64, mono);
        }
        self.speaker.end = pos + frames as u64;
        stats.callbacks_out += 1;
        stats.frames_out += frames as u64;
        self.out.advance();
    }

    #[allow(clippy::too_many_arguments)]
    fn run_in(
        &mut self,
        cfg: &FakeConfig,
        latency_s: f64,
        gain: f32,
        voices: &[Voice],
        external: &mut Option<ExternalFn>,
        stats: &mut FakeStats,
    ) {
        let frames = self.inp.frames as usize;
        let q0 = self.inp.pos;
        let q1 = q0 + frames as u64;
        let mic = &mut self.mic[..frames];

        // Input position Q is captured at t_in0 + Q/rate_in; the sound left the speaker
        // latency_s earlier, at output position (that time − t_out0)·rate_out.
        let ratio = self.out.line.rate / self.inp.line.rate;
        let phi0 = (self.inp.line.t0 - self.out.line.t0 - latency_s) * self.out.line.rate;
        if gain == 0.0 || (!self.direct && self.ir.is_empty()) {
            // Gain 0, or a response of zeros only: nothing arrives.
            mic.fill(0.0);
        } else if self.direct {
            for (i, m) in mic.iter_mut().enumerate() {
                let phi = phi0 + (q0 + i as u64) as f64 * ratio;
                *m = gain * self.speaker.sample(phi, stats);
            }
        } else {
            // The response also needs the path signal of frames lost in an xrun.
            let from = self.x_end.max(q0.saturating_sub(self.ir_len));
            for q in from..q1 {
                let phi = phi0 + q as f64 * ratio;
                self.xring[q as usize & self.xmask] = self.speaker.sample(phi, stats);
            }
            self.x_end = q1;
            for (i, m) in mic.iter_mut().enumerate() {
                let q = q0 + i as u64;
                let mut acc = 0.0f32;
                for &(j, c) in &self.ir {
                    if q >= j {
                        acc += c * self.xring[(q - j) as usize & self.xmask];
                    }
                }
                *m = gain * acc;
            }
        }

        if cfg.path.noise_rms > 0.0 {
            for m in mic.iter_mut() {
                *m += self.noise.normal() as f32 * cfg.path.noise_rms;
            }
        }
        for v in voices {
            let end = v.start + v.samples.len() as u64;
            for q in v.start.max(q0)..end.min(q1) {
                mic[(q - q0) as usize] += v.gain * v.samples[(q - v.start) as usize];
            }
        }
        if let Some(f) = external.as_mut() {
            f(q0, mic);
        }
        if cfg.clip {
            for m in mic.iter_mut() {
                *m = m.clamp(-1.0, 1.0);
            }
        }

        let ch = self.inp.ch;
        let buf = &mut self.in_buf[..frames * ch];
        for (frame, m) in buf.chunks_exact_mut(ch).zip(mic.iter()) {
            frame.fill(*m);
        }
        let shift_ns = if cfg.raw_input_stamps {
            cfg.origin_offset_ns
        } else {
            0
        };
        let stamp = self.inp.stamp(cfg.host_origin_ns, shift_ns);
        if std::mem::take(&mut self.inp.notify_xrun) {
            self.notify_xrun(cfg, Side::In, stats);
        }
        (self.io.input)(&self.in_buf[..frames * ch], stamp);
        stats.callbacks_in += 1;
        stats.frames_in += frames as u64;
        self.inp.advance();
    }
}

// ------------------------------------------------------------------ core

struct Core {
    cfg: FakeConfig,
    now: f64,
    latency_s: f64,
    gain: f32,
    session: Option<Box<Session>>,
    events: [BackendEvent; MAX_EVENTS],
    n_events: usize,
    /// A fatal event injected while the queue was full: delivered after the queue.
    fatal: Option<BackendEvent>,
    voices: Vec<Voice>,
    external: Option<ExternalFn>,
    stats: FakeStats,
}

impl Core {
    fn deliver_events(&mut self) {
        let n = std::mem::take(&mut self.n_events);
        let fatal = self.fatal.take();
        let Some(s) = self.session.as_mut() else {
            return;
        };
        let events = self.events;
        for ev in events[..n].iter().chain(fatal.iter()) {
            if s.dead {
                break;
            }
            (s.io.error)(*ev, None);
            self.stats.events += 1;
            if matches!(ev, BackendEvent::Invalidated | BackendEvent::DeviceLost) {
                s.dead = true;
            }
        }
    }

    /// Runs the next callback if it is due by `until`.
    fn step(&mut self, until: f64) -> Option<Side> {
        self.deliver_events();
        let Core {
            cfg,
            now,
            latency_s,
            gain,
            session,
            voices,
            external,
            stats,
            ..
        } = self;
        let s = session.as_mut().filter(|s| !s.dead)?;
        let (side, t) = s.next();
        // Delayed xrun notices due before that callback (the dispatcher runs first on a tie).
        if let Some(due) = s.deliver_late(t.min(until), MAX_LATE, stats) {
            *now = now.max(due);
        }
        if t > until {
            return None;
        }
        *now = now.max(t);
        match side {
            Side::Out => s.run_out(cfg, stats),
            Side::In => s.run_in(cfg, *latency_s, *gain, voices, external, stats),
        }
        Some(side)
    }

    fn pump(&mut self, seconds: f64) -> usize {
        let until = self.now + seconds.max(0.0);
        let mut ran = 0;
        while self.step(until).is_some() {
            ran += 1;
        }
        self.now = self.now.max(until);
        ran
    }
}

impl Core {
    fn now_ns(&self) -> u64 {
        self.cfg.host_origin_ns + (self.now * 1.0e9).round() as u64
    }
}

fn lock(core: &Mutex<Core>) -> MutexGuard<'_, Core> {
    core.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The fake backend. Clones share the same devices and clock.
#[derive(Clone)]
pub struct FakeBackend {
    core: Arc<Mutex<Core>>,
}

impl FakeBackend {
    /// A backend at true time zero with no stream open. The configuration is checked
    /// by `open`.
    pub fn new(cfg: FakeConfig) -> Self {
        let core = Core {
            now: 0.0,
            latency_s: cfg.path.latency_s,
            gain: cfg.path.gain,
            session: None,
            events: [BackendEvent::Other; MAX_EVENTS],
            n_events: 0,
            fatal: None,
            voices: Vec::with_capacity(MAX_VOICES),
            external: None,
            stats: FakeStats::default(),
            cfg,
        };
        Self {
            core: Arc::new(Mutex::new(core)),
        }
    }

    /// The control side: pump, inject, inspect.
    pub fn handle(&self) -> FakeHandle {
        FakeHandle {
            core: Arc::clone(&self.core),
        }
    }
}

impl Backend for FakeBackend {
    type Streams = FakeStreams;

    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        Ok(vec![
            DeviceInfo {
                id: FAKE_INPUT_ID.into(),
                name: "Fake microphone".into(),
                is_input: true,
                is_output: false,
                is_default: true,
            },
            DeviceInfo {
                id: FAKE_OUTPUT_ID.into(),
                name: "Fake loudspeaker".into(),
                is_input: false,
                is_output: true,
                is_default: true,
            },
        ])
    }

    /// Opens both streams at the current virtual time. One pair at a time; after the
    /// streams are dropped the backend can be opened again (frame counters restart).
    fn open(
        &self,
        req: &OpenRequest,
        io: IoCallbacks,
    ) -> Result<(Self::Streams, StreamInfo), AudioError> {
        let mut core = lock(&self.core);
        if core.session.is_some() {
            return Err(AudioError::Backend("fake streams are already open".into()));
        }
        for (asked, have, what) in [
            (&req.input_id, FAKE_INPUT_ID, "input"),
            (&req.output_id, FAKE_OUTPUT_ID, "output"),
        ] {
            if asked.as_deref().is_some_and(|id| id != have) {
                return Err(AudioError::NoDevice(format!(
                    "{what} {}",
                    asked.as_deref().unwrap_or_default()
                )));
            }
        }
        core.cfg.validate()?;
        if req.buffer_frames.is_some_and(|n| n == 0 || n > MAX_BLOCK) {
            return Err(AudioError::Backend(
                "buffer_frames must be in 1..=65536".into(),
            ));
        }
        core.stats.opens += 1;
        let id = core.stats.opens;
        let session = Session::new(&core.cfg, req, io, core.now, id);
        core.voices.clear();
        core.n_events = 0;
        core.fatal = None;
        core.session = Some(Box::new(session));
        let info = StreamInfo {
            fs_in: core.cfg.fs_in,
            fs_out: core.cfg.fs_out,
            ch_in: core.cfg.ch_in,
            ch_out: core.cfg.ch_out,
            in_id: FAKE_INPUT_ID.into(),
            out_id: FAKE_OUTPUT_ID.into(),
            origin_offset_ns: core.cfg.origin_offset_ns,
        };
        let streams = FakeStreams {
            core: Arc::clone(&self.core),
            id,
        };
        Ok((streams, info))
    }
}

/// The open fake streams. Dropping them closes the streams: no callback runs afterwards
/// and the callbacks are dropped.
pub struct FakeStreams {
    core: Arc<Mutex<Core>>,
    id: u64,
}

impl HostClock for FakeStreams {
    /// The virtual host clock: the same reading as [`FakeHandle::now_ns`]. It locks the
    /// backend, so it must not be called from inside a callback.
    fn host_now_ns(&self) -> u64 {
        lock(&self.core).now_ns()
    }
}

impl Drop for FakeStreams {
    fn drop(&mut self) {
        let mut core = lock(&self.core);
        if core.session.as_ref().is_some_and(|s| s.id == self.id) {
            let session = core.session.take();
            let voices = std::mem::replace(&mut core.voices, Vec::with_capacity(MAX_VOICES));
            drop(core);
            // Outside the lock: dropping the callbacks may run foreign code.
            drop(session);
            drop(voices);
        }
    }
}

/// Control side of a [`FakeBackend`]. Clones share the backend.
#[derive(Clone)]
pub struct FakeHandle {
    core: Arc<Mutex<Core>>,
}

impl FakeHandle {
    /// Advances virtual time by `seconds` and runs, in time order, every callback due
    /// in that span. Returns how many ran. This is the deterministic manual pump.
    pub fn pump(&self, seconds: f64) -> usize {
        lock(&self.core).pump(seconds)
    }

    /// Runs exactly the next callback, advancing virtual time to it. `None` when no
    /// stream is delivering.
    pub fn pump_next(&self) -> Option<Side> {
        lock(&self.core).step(f64::INFINITY)
    }

    /// True time, seconds since the backend was created.
    pub fn now_s(&self) -> f64 {
        lock(&self.core).now
    }

    /// The host clock now, ns: what a stream's `now()` would read.
    pub fn now_ns(&self) -> u64 {
        lock(&self.core).now_ns()
    }

    /// Streams are open (they may have stopped after a fatal device event).
    pub fn is_open(&self) -> bool {
        lock(&self.core).session.is_some()
    }

    /// Streams are open and delivering callbacks.
    pub fn is_running(&self) -> bool {
        lock(&self.core).session.as_ref().is_some_and(|s| !s.dead)
    }

    /// Counters.
    pub fn stats(&self) -> FakeStats {
        lock(&self.core).stats
    }

    /// The exact frame/time relation of the open streams. `None` when closed.
    pub fn truth(&self) -> Option<FakeTruth> {
        let core = lock(&self.core);
        let s = core.session.as_ref()?;
        Some(FakeTruth {
            fs_in: f64::from(core.cfg.fs_in),
            fs_out: f64::from(core.cfg.fs_out),
            latency_s: core.latency_s,
            inp: s.inp.line.clone(),
            out: s.out.line.clone(),
        })
    }

    /// Next callback frame of each stream: `(input, output)`. `None` when closed.
    pub fn next_frames(&self) -> Option<(u64, u64)> {
        let core = lock(&self.core);
        core.session.as_ref().map(|s| (s.inp.frame, s.out.frame))
    }

    /// The device of `side` runs `frames` frames before its next block: an overrun
    /// (input: captured, never delivered) or an underrun (output: silence played). The
    /// error callback gets `XrunIn` / `XrunOut` just before that block (or
    /// [`FakeConfig::error_delay_s`] later), unless [`FakeConfig::notify_out_xrun`] is off
    /// for the output. Returns `false` when no stream is delivering.
    pub fn inject_xrun(&self, side: Side, frames: u32) -> bool {
        let mut core = lock(&self.core);
        let notify_out = core.cfg.notify_out_xrun;
        let Core { session, stats, .. } = &mut *core;
        let Some(s) = session.as_mut().filter(|s| !s.dead) else {
            return false;
        };
        let lost = u64::from(frames);
        match side {
            Side::In => {
                s.inp.add_gap(lost);
                s.inp.notify_xrun = true;
                stats.xruns_in += 1;
            }
            Side::Out => {
                s.speaker.silence(s.out.pos, lost);
                s.out.add_gap(lost);
                s.speaker.end = s.out.pos;
                s.out.notify_xrun = notify_out;
                stats.xruns_out += 1;
            }
        }
        true
    }

    /// Delivers `ev` to the error callback before the next callback. `Invalidated` and
    /// `DeviceLost` then stop both streams for good; anything else only notifies.
    /// At most 16 events wait at a time. Further ones are discarded and counted in
    /// [`FakeStats::events_dropped`], except a fatal one: it is kept and delivered after
    /// the queue.
    pub fn inject_event(&self, ev: BackendEvent) {
        let mut core = lock(&self.core);
        let fatal = matches!(ev, BackendEvent::Invalidated | BackendEvent::DeviceLost);
        if core.n_events < MAX_EVENTS {
            let n = core.n_events;
            core.events[n] = ev;
            core.n_events += 1;
        } else if fatal && core.fatal.is_none() {
            core.fatal = Some(ev);
        } else if !fatal {
            core.stats.events_dropped += 1;
        }
    }

    /// Adds `offset_s` to the `device_ns` of the next stamp of `side` (once).
    pub fn inject_stamp_outlier(&self, side: Side, offset_s: f64) {
        let mut core = lock(&self.core);
        if let Some(s) = core.session.as_mut() {
            match side {
                Side::In => s.inp.outlier_s = offset_s,
                Side::Out => s.out.outlier_s = offset_s,
            }
        }
    }

    /// Changes the path latency from the next input block on (clamped to
    /// `0..=max_latency_s`).
    pub fn set_latency_s(&self, seconds: f64) {
        let mut core = lock(&self.core);
        core.latency_s = seconds.clamp(0.0, core.cfg.path.max_latency_s);
    }

    /// Changes the path gain from the next input block on. 0 is silence.
    pub fn set_path_gain(&self, gain: f32) {
        lock(&self.core).gain = gain;
    }

    /// Adds `samples` (mono, at the input rate) to the microphone, starting at true
    /// time `at_s` rounded to a whole input frame: a virtual guitar. Returns the input
    /// callback frame of the first sample, or `None` if the streams are closed, `at_s`
    /// is before the input started, or 64 sounds are already pending.
    pub fn play_external(&self, at_s: f64, samples: Arc<[f32]>, gain: f32) -> Option<u64> {
        let mut core = lock(&self.core);
        let Core {
            session, voices, ..
        } = &mut *core;
        let s = session.as_ref()?;
        let done = s.inp.pos;
        voices.retain(|v| v.start + v.samples.len() as u64 > done);
        let pos = ((at_s - s.inp.line.t0) * s.inp.line.rate).round();
        if pos < 0.0 || voices.len() >= MAX_VOICES {
            return None;
        }
        let start = pos as u64;
        voices.push(Voice {
            start,
            samples,
            gain,
        });
        Some(s.inp.line.frame_of_pos(start as f64) as u64)
    }

    /// Installs (or removes) a function that adds to every microphone block.
    pub fn set_external_fn(&self, f: Option<ExternalFn>) {
        let old = std::mem::replace(&mut lock(&self.core).external, f);
        drop(old);
    }

    /// Pumps from a thread, `speed` virtual seconds per wall-clock second, until the
    /// returned guard is dropped. For demos and time-out tests; results then depend on
    /// the scheduler, so deterministic tests use [`FakeHandle::pump`].
    pub fn spawn_realtime(&self, speed: f64) -> FakeRealtime {
        let stop = Arc::new(AtomicBool::new(false));
        let (handle, flag) = (self.clone(), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            let start = Instant::now();
            let mut done = 0.0;
            while !flag.load(Ordering::Relaxed) {
                let want = start.elapsed().as_secs_f64() * speed;
                if want > done {
                    handle.pump(want - done);
                    done = want;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        FakeRealtime {
            stop,
            thread: Some(thread),
        }
    }
}

/// Guard of [`FakeHandle::spawn_realtime`]: stops and joins the pump thread on drop.
pub struct FakeRealtime {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for FakeRealtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_rows_sum_to_one_and_are_symmetric() {
        for ratio in [1.0, 44_100.0 / 48_000.0, 48_000.0 / 44_100.0] {
            let t = kernel_table(ratio);
            assert_eq!(t.len(), (PHASES + 1) * TAPS);
            for k in 0..=PHASES {
                let sum: f32 = t[k * TAPS..(k + 1) * TAPS].iter().sum();
                assert!((sum - 1.0).abs() < 1.0e-5, "row {k}: {sum}");
            }
            // Row for frac and row for 1 − frac mirror each other.
            for k in 0..=PHASES {
                for j in 0..TAPS {
                    let a = t[k * TAPS + j];
                    let b = t[(PHASES - k) * TAPS + (TAPS - 1 - j)];
                    assert!((a - b).abs() < 1.0e-6, "k {k} j {j}: {a} vs {b}");
                }
            }
            // The last row is the first one shifted by a frame.
            for j in 0..TAPS - 1 {
                assert!((t[PHASES * TAPS + j + 1] - t[j]).abs() < 1.0e-6);
            }
        }
    }

    #[test]
    fn timeline_with_gaps_round_trips() {
        let mut line = Timeline {
            rate: 48_000.0,
            t0: 1.0,
            base: 0,
            gaps: Vec::new(),
        };
        line.push_gap(1000, 480);
        line.push_gap(5000, 20);
        assert_eq!(line.pos_of_frame(999.0), 999.0);
        assert_eq!(line.pos_of_frame(1000.0), 1480.0);
        assert_eq!(line.pos_of_frame(5000.0), 5500.0);
        assert_eq!(line.frame_of_pos(999.5), 999.5);
        assert_eq!(line.frame_of_pos(1200.0), 1000.0, "inside the hole");
        assert_eq!(line.frame_of_pos(1480.0), 1000.0);
        assert_eq!(line.frame_of_pos(5479.0), 4999.0);
        assert_eq!(line.frame_of_pos(5490.0), 5000.0, "inside the second hole");
        assert_eq!(line.frame_of_pos(6000.0), 5500.0);
        for f in [0.0, 999.0, 1000.0, 4999.5, 5000.0, 9000.25] {
            let t = line.time_of_frame(f);
            assert!((line.frame_at(t) - f).abs() < 1.0e-6, "frame {f}");
        }
    }

    #[test]
    fn block_models() {
        let mut g = BlockGen {
            model: BlockModel::Uniform { min: 3, max: 7 },
            rng: Mulberry32::new(9),
            idx: 0,
        };
        let mut seen = [false; 8];
        for _ in 0..500 {
            let n = g.next() as usize;
            assert!((3..=7).contains(&n));
            seen[n] = true;
        }
        assert!(seen[3..=7].iter().all(|s| *s));
        g.model = BlockModel::Cycle(vec![480, 448, 512]);
        let got: Vec<u32> = (0..5).map(|_| g.next()).collect();
        assert_eq!(got, [480, 448, 512, 480, 448]);
        assert_eq!(BlockModel::Uniform { min: 9, max: 2 }.bounds(), None);
        assert_eq!(BlockModel::Cycle(vec![]).bounds(), None);
    }
}
