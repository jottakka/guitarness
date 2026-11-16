//! Plain data types shared across the crate.

use std::fmt;

/// Frames delivered to the input callback since the stream started.
pub type InFrame = u64;
/// Frames requested by the output callback since the stream started.
pub type OutFrame = u64;

/// Position and timing of one callback block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// Frame index of the first frame of the block.
    pub first: u64,
    /// Frames in the block.
    pub frames: u32,
    /// Device time in ns: `capture` (input) or `playback` (output).
    pub device_ns: u64,
    /// Time of the callback itself, in ns.
    pub callback_ns: u64,
}

/// Stream or device event reported by a backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendEvent {
    /// Input overrun.
    XrunIn,
    /// Output underrun.
    XrunOut,
    /// Stream invalidated.
    Invalidated,
    /// Device disappeared.
    DeviceLost,
    /// Default device changed.
    DeviceChanged,
    /// The OS refused real-time priority.
    RealtimeDenied,
    /// Any other error.
    Other,
}

/// Which side of the duplex pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Input (capture).
    In,
    /// Output (playback).
    Out,
}

/// Which input channel feeds the analysis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelPick {
    /// One channel by index (default: 0).
    Index(u16),
    /// Average of all channels.
    Mean,
}

impl Default for ChannelPick {
    fn default() -> Self {
        Self::Index(0)
    }
}

/// A device the backend can open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable identifier used in [`OpenRequest`].
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Has input channels.
    pub is_input: bool,
    /// Has output channels.
    pub is_output: bool,
    /// Whether this is the system default for its direction(s).
    pub is_default: bool,
}

/// What to open. `None` ids mean the system default device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenRequest {
    /// Input device id.
    pub input_id: Option<String>,
    /// Output device id.
    pub output_id: Option<String>,
    /// Fixed buffer size in frames; `None` = backend default.
    pub buffer_frames: Option<u32>,
}

/// Actual stream parameters after opening.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    /// Input sample rate in Hz.
    pub fs_in: u32,
    /// Output sample rate in Hz.
    pub fs_out: u32,
    /// Input channels.
    pub ch_in: u16,
    /// Output channels.
    pub ch_out: u16,
    /// Opened input device id.
    pub in_id: String,
    /// Opened output device id.
    pub out_id: String,
    /// Offset between the input and output device clock origins, in ns.
    pub origin_offset_ns: i64,
}

/// Errors from the audio layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioError {
    /// No usable device.
    NoDevice(String),
    /// Sample rate not supported (only 44.1 and 48 kHz).
    UnsupportedRate(u32),
    /// Sample format not supported.
    UnsupportedFormat(String),
    /// The backend failed.
    Backend(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice(what) => write!(f, "no audio device: {what}"),
            Self::UnsupportedRate(hz) => {
                write!(f, "unsupported sample rate {hz} Hz (use 44100 or 48000)")
            }
            Self::UnsupportedFormat(fmt_) => write!(f, "unsupported sample format: {fmt_}"),
            Self::Backend(msg) => write!(f, "audio backend error: {msg}"),
        }
    }
}

impl std::error::Error for AudioError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        assert_eq!(
            AudioError::UnsupportedRate(96000).to_string(),
            "unsupported sample rate 96000 Hz (use 44100 or 48000)"
        );
        assert_eq!(
            AudioError::NoDevice("input".into()).to_string(),
            "no audio device: input"
        );
    }

    #[test]
    fn defaults() {
        assert_eq!(ChannelPick::default(), ChannelPick::Index(0));
        assert_eq!(OpenRequest::default().buffer_frames, None);
    }
}
