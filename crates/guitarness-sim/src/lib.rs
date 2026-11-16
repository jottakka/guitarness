//! Virtual guitar and scenarios: synthetic strings, strums, rooms and microphones fed through
//! the real DSP code, with ground truth for the benches.
//!
//! - [`string`]: plucked string (KS with loop low-pass, pick comb, damping, re-fret).
//! - [`strum`]: voicings, strum order and spread, muted strums, whole takes.
//! - [`fx`]: pink noise, hum, metronome click (> 6 kHz), reverb, laptop mic.
//! - [`timing`]: onset timing bench and no-strum regressions (DESIGN §1.11).
pub use guitarness_dsp::synth;

pub mod fx;
pub mod string;
pub mod strum;
pub mod timing;
