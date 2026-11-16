//! Tônica real-time DSP core. Engine-independent; no allocation on the per-frame path
//! after construction.
//!
//! - [`biquad`]: input filters (RBJ).
//! - [`frontend`]: decimation, ring window, framing, per-target presence.
//! - [`detector`]: McLeod pitch method (NSDF), direct or FFT autocorrelation.
//! - [`tracker`]: frames → attack / note events.
//! - [`onset`]: strum onsets with ~1 ms timing (spectral flux + energy retro-dating).
//! - [`resample`]: half-band decimation by 2.
//! - [`latency`]: loopback latency (`L_rt`) and player-bias calibration (offline).
//! - [`synth`]: Karplus-Strong plucked string for tests and the virtual guitar.

pub mod biquad;
pub mod detector;
pub mod frontend;
pub mod latency;
pub mod onset;
pub mod pipeline;
pub mod resample;
pub mod synth;
pub mod tracker;

pub use detector::{AcfMode, Analysis, Detector};
pub use frontend::{Frame, Frontend, FrontendConfig, MAX_TARGETS};
pub use onset::{OnsetConfig, OnsetDetector, OnsetEvent, Strum};
pub use pipeline::Pipeline;
pub use tracker::{Event, Tracker, TrackerParams, Via};

/// MIDI note number to frequency (A4 = `a4` Hz).
#[inline]
pub fn mtof(midi: f64, a4: f64) -> f64 {
    a4 * 2f64.powf((midi - 69.0) / 12.0)
}
