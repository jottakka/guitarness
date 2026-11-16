//! Live audio for Tonica: backend abstraction, mixer, clocks and capture.
//!
//! Only `cpal_backend` may import `cpal`; the rest of the crate is backend
//! agnostic.

pub mod backend;
pub mod calib;
pub mod capture;
pub mod clock;
pub mod cpal_backend;
pub mod engine;
pub mod fake;
pub mod mixer;
pub mod playhead;
pub mod sounds;
pub mod stats;
pub mod types;

pub use backend::{Backend, ErrorFn, HostClock, InputFn, IoCallbacks, OutputFn, XrunBoundary};
pub use calib::{
    BeatPlan, CalibConfig, CalibUpdate, Calibrator, Details, Outcome, Profile, ProfileKey, Prompt,
    QuickCheck, RoundDetails, RoundKind, Warning, find_profile, profiles_from_json,
    profiles_to_json, upsert_profile,
};
pub use capture::{CaptureOutputs, CaptureRings, CaptureStage, CaptureStats, InEvent};
pub use clock::{
    ChartClock, ClockConfig, ClockMap, ClockQuality, ClockSnapshot, ClockStep, ClockStreamStats,
    LateNotice, StampSource, StepCause,
};
pub use cpal_backend::{CpalBackend, CpalStreams};
pub use engine::{
    AudioEngine, EngineConfig, EngineEvent, EngineIo, MixerHandle, OnsetNote, PitchNote, Pump,
    TapHandle,
};
pub use fake::{
    BlockModel, ExternalFn, FakeBackend, FakeConfig, FakeHandle, FakePath, FakeRealtime, FakeStats,
    FakeStreams, FakeTruth,
};
pub use mixer::{MixCmd, MixEvent, Mixer, VOICES};
pub use sounds::{SoundBank, SoundId};
pub use stats::{Drops, Ring, Stats};
pub use types::{
    AudioError, BackendEvent, ChannelPick, DeviceInfo, InFrame, OpenRequest, OutFrame, Side, Stamp,
    StreamInfo,
};
