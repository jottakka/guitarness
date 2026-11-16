//! Live calibration: the round-trip latency `L_rt` of the system, kept apart from what
//! the strums say about the player (M2 plan §4, Pulso plan §2.1 and §2.2).
//!
//! [`Calibrator`] is a state machine with no user interface, no wall clock and no file
//! access. A driver (the CLI, the session of the game) calls [`Calibrator::step_io`] (or
//! [`Calibrator::step`] with the events it drained itself) every few milliseconds, shows
//! the [`Prompt`] of the returned [`CalibUpdate`] as a short text without numbers, and
//! stops when `done` is set. The numbers, in ms, are in [`Calibrator::details`]. After
//! [`Outcome::Kept`] or [`Outcome::Calibrated`] the driver saves the profile and calls
//! `AudioEngine::clear_calibration_stale`.
//!
//! Everything here runs on the control thread: it allocates and it runs FFTs (the analysis
//! of a probe round takes a few milliseconds), never on an audio callback.
//!
//! # System and player
//!
//! A judge computes `e = (t_onset − in_of_out(f_beat)) − off`. What `off` may be is the
//! [`Basis`] of the profile ([`Profile::basis`], [`Profile::judge_offset_ms`]):
//!
//! - [`Basis::Loopback`]: the acoustic loop was measured. `off = L_rt` and nothing else.
//!   The player's own tendency is never part of `off`: subtracting it would make the
//!   player's average the beat. Such a profile is judgeable whatever the strums look
//!   like (a scattered player is exactly who timing practice is for). When step 5 runs
//!   anyway ([`BeatStep::Always`]), the median strum error after `L_rt` is recorded as
//!   [`Profile::tendency_ms`]: an observation of that step, not a correction.
//! - [`Basis::Estimate`]: no loop (headphones, Bluetooth). The strums of step 5 give
//!   one number, [`Profile::combined_ms`]: latency plus tendency, which cannot be told
//!   apart. Timing is judged against it if the strums are consistent and the reading is
//!   unambiguous (next but one section); a tendency can never be claimed from it.
//! - [`Basis::None`]: nothing to judge timing with. The rhythm is shown, not judged.
//!
//! # Steps and timing (seconds of output time)
//!
//! | step | what | time |
//! |---|---|---|
//! | 0 | saved profile of this device pair: 3 probes; ΔL_rt within ±2 ms keeps it | clock lock + 1.3 |
//! | 1 | silence: noise floor, clock lock, all-zero input, clipping | 3.0 (short: 1.0 after a tap reset of 0.25) |
//! | 2s | only [`CalibConfig::short`]: 6 probes at −12 dBFS, 300 ± 40 ms apart; 5 heard with MAD ≤ 3 ms give `L_rt`, anything else goes on to step 2 | 2.2 |
//! | 2 | 10 probes at −12 dBFS, 450 ± 40 ms apart, tap recording | ≤ 5.1 |
//! | 3 | analysis: robust line through the clock snapshots, matched filter | inside a `step` |
//! | 4 | too few probes heard: one more round at −6 dBFS; refused again: no `L_rt` | + ≤ 5.1 |
//! | 5 | [`CalibConfig::beat_step`] says when. 4 count-in clicks, up to 16 beats at 80 BPM of muted strums; stops at 12 paired strums with MAD ≤ 15 ms | ≤ 15.3 |
//! | 6 | the [`Profile`] | — |
//!
//! With the default [`BeatStep::WhenNoLoop`] a measured loop ends the calibration after
//! step 3: at most 8.1 s, and about 3.4 s with [`CalibConfig::short`] on a clock that is
//! already locked (about 4.2 s right after the streams opened: the clock takes 2 s to
//! lock). Step 5 then only runs where there is no loop. With step 5 the nominal run is
//! at most 23.4 s (24.8 s when a failed quick check comes first); with the −6 dBFS
//! round, one repeat after an audio glitch or the clipping re-measure (1.5 s) it stays
//! below 30 s; those three together can reach 35 s, and a refused short round adds its
//! 2.2 s. A change of stamp source during step 5 (see "Clocks") runs steps 2 to 5 once
//! more: the time already spent in step 5 plus up to 5.3 s for the clock to lock again.
//! A probe round that is repeated because the clock lost its lock waits as long.
//!
//! # The short round
//!
//! The windows searched for two probes 300 ms apart overlap, so a window can hold a
//! neighbour of its probe: the probe before it once the latency passes about 210 ms, the
//! probe after it when its own was not heard. Such a reading is off by the time between
//! the two probes. And the probes are one sound, so agreement between the windows
//! proves nothing: a latency that moves from probe to probe can put five arrivals at
//! the same place in the windows of their neighbours. The short round therefore counts
//! only the readings it can vouch for, from the first probe on. A reading that agrees
//! with the first within the MAD limit (3 ms), and lies with that margin to spare before
//! the earliest time a later probe can arrive in its window (the shortest gap after the
//! window start), is the arrival of its own probe, as long as every probe before it was
//! read so (their arrivals are then before its window). The first reading that
//! disagrees, or a probe not heard, ends the count. Fewer than
//! [`CalibConfig::short_min_found`] such readings, and the full round decides, whose
//! probes are far enough apart; so it does from a latency of about 205 ms up (260 ms
//! less the 50 ms pre-roll and twice the limit). The gaps are spread evenly over
//! ± [`CalibConfig::short_gap_jitter_s`] (20 ms apart) in a random order.
//!
//! # No failure screen
//!
//! Only two outcomes stop without a profile: [`Outcome::MicBlocked`] (the input is digital
//! silence even while a probe plays) and [`Outcome::Interrupted`] (the device went away,
//! or the mixer does not answer). Everything else ends in a profile:
//!
//! - probes not heard, or their latency unstable (headphones, Bluetooth): no `L_rt`.
//!   Step 5 then gives `combined_ms` and the rhythm is still judged if the strums are
//!   consistent and the offset is known (next section);
//! - without a loop, strums too few or too scattered (MAD > 15 ms), or no step 5 at all
//!   ([`BeatStep::Never`]): `judgeable = false`, the prompt is [`Prompt::RhythmLater`]
//!   and the UI offers the beat step again later ([`CalibConfig::retry_rhythm`]).
//!
//! [`Outcome::Unstable`] (the clock map never locked, or it changed twice under step 5)
//! leaves a saved profile untouched.
//!
//! # Without a loop: which beat a strum answers
//!
//! With `L_rt`, a strum is paired with the click it answers. Without it, the strums only
//! give the offset on the beat grid, and an offset `r` reads the same as `r` plus whole
//! beats (at 80 BPM, 700 ms reads as 50 ms early). Judging with the wrong one pairs every
//! strum with the wrong beat as soon as the tempo is another. So the reading is used
//! only if:
//!
//! - the strums fit it better than they fit one, two or three beats more: fewer strums
//!   without a beat and fewer beats without a strum (the first scored beat is the anchor:
//!   a strum on it has no beat under the later readings; a strum after the last beat has
//!   one only under them). A tie is not a fit: a player who misses the first scored beat
//!   plays exactly what a player 750 ms later plays, and gets "not yet"
//!   ([`Warning::OffsetUnresolved`]);
//! - it is at most [`CalibConfig::a3_max_offset_ms`] ([`Warning::OffsetBeyondCeiling`]).
//!
//! Otherwise the profile has no `combined_ms` and `judgeable = false`, with the same
//! [`Prompt::RhythmLater`] as scattered strums; [`Details::unresolved_offset_ms`] keeps
//! the reading. This errs on the side of "not yet", which step 5 repairs when the UI
//! offers it again ([`CalibConfig::retry_rhythm`]), where a wrong offset would stay in
//! the profile unseen: without a loop, no quick check can find it.
//!
//! # Clocks
//!
//! - **Output frames.** The calibrator reads the mixer's own frame counter: it sends a
//!   silent `Play` far in the past and the mixer answers `Late { by }`, where `at + by` is
//!   the first frame of the block that took the command (a sound entirely in the past
//!   takes no voice). No driver clock is needed, and counters that restart on a reopen or
//!   callbacks skipped at start cannot shift the schedule. When the driver stalls (0.3 s
//!   or more of input time between two steps) the counter in hand is old: nothing is
//!   scheduled or timed until a ping sent after the stall is answered.
//! - **Tap samples.** `TapHandle::origin` is detector time (frames the capture stage
//!   processed), not an input frame: the two differ by the frames the backend delivered
//!   before the stage was installed. The difference `t_in − detector time` is read from
//!   the `Pitch` and `Onset` events, which carry both. No `L_rt` is produced without it.
//!   An origin older than the moment the tap was switched on belongs to an earlier
//!   recording: the tap is then reset (twice at most) instead of being used.
//! - **Emission times.** `emits[k] = in_of_out(at_k) − first recorded frame`. The raw
//!   stamps stay inside the engine, so `in_of_out` is a line fitted through the
//!   [`ClockSnapshot`]s of the probe window, each evaluated at the frame it was received
//!   at: least squares, then a second pass without the residuals beyond 3·MAD. The line
//!   averages the loop noise over the whole window without the lag of a causal filter,
//!   and a snapshot from a transient cannot pull it. The same frames mapped with the
//!   snapshot that was current when each probe played ("live") must agree within 0.3 ms;
//!   if not, [`Warning::ClockFitMismatch`] is recorded and the fit is still used. A round
//!   starts on a locked map. A clock step, a snapshot that is not locked or a tap
//!   overflow inside the window discards the round, which is repeated once, when the map
//!   is locked again (it gets 5 s for that if it was not locked when the round ended).
//! - **Stamp source.** When the clock map gives up the device stamps of a stream for the
//!   callback times ([`StepCause::Fallback`]), `in_of_out` moves by a constant: the time
//!   between a callback and its sound. `L_rt` and what the strums gave are only valid on
//!   the map they were measured on. A fallback during a probe round discards the round,
//!   and one while a round waits for the lock gives the map 5 s to lock on the new
//!   stamps: with a loop and the default [`BeatStep`] the round is the last step, so the
//!   latency it saves must be the one of the map in force at its end.
//!   A fallback during step 5 discards the pairs so far *and* the `L_rt` from before
//!   (also when the strums are only an observation: the profile is saved with the
//!   stamps in force at the end, and its latency must hold on them): steps 2 to 5
//!   run again, once (the budget of one repeat is shared with the rounds; a second
//!   change ends with [`Outcome::Unstable`]). A beat is only used when the map was locked
//!   as it played, and step 5 does not end on a clock step whose unjudged window has not
//!   arrived. The profile records the source of both sides ([`Profile::stamps_in`],
//!   [`Profile::stamps_out`]); the quick check does not keep a profile made on another
//!   source, and a driver that sees a fallback later must ask [`Profile::holds_on`].
//! - **Lock at the end.** A profile is judgeable only if the map is locked when it is
//!   made. Step 5 is long and no probe round watches the map during it: one that is
//!   acquiring again when the step ends gets 5 s to lock (the prompt is
//!   [`Prompt::Quiet`]); still unlocked, the profile is saved with `judgeable = false`.
//!   A fallback in that wait is one during step 5.
//! - **Epoch.** The event queue outlives `AudioEngine::reopen`: a calibrator built on the
//!   fresh handles first reads what the streams before left there, then `Reopened`.
//!   Until the mixer answers the first ping, the newest epoch seen is taken as the one
//!   the calibration runs on, older events are skipped and a `Stopped` is only believed
//!   if no `Reopened` follows it within a few steps. After the first answer, a newer
//!   epoch or a `Stopped` interrupts.
//! - **Commands.** Sounds wait in a queue of the calibrator and leave in order as the
//!   mixer queue has room (`EngineConfig::cmd_ring` may be as small as 1); the ping uses
//!   a slot only when nothing waits. A sound that cannot leave 30 ms before its frame is
//!   left out ([`Warning::QueueFull`]) and the round is analysed with the others.
//! - **No answer.** Every wait of the state machine is counted on the mixer's frames, so
//!   a mixer that never answers would hold it for ever. It does not: after 4 s of input
//!   time or [`CalibConfig::max_silent_steps`] steps without an answer, or as soon as the
//!   handle refuses a command with room in its queue (the engine behind it is gone), the
//!   calibration ends with [`Outcome::Interrupted`] and [`Warning::NoMixer`].
//! - **Negative latency.** The search starts [`CalibConfig::pre_roll_s`] before the
//!   stamped emission: stamps that overstate the output latency give a small negative
//!   `L_rt`, which is as valid as a positive one in `e = (t_onset − in_of_out(f_beat)) −
//!   L_rt`.
//!
//! # Levels
//!
//! Probes play at −12 dBFS, the second round at −6 dBFS, the accented click at −8 dBFS and
//! the plain click 1 to 3 dB under it (one gain for both). A level is the true peak of the
//! sound: the band-limited signal between the samples, which the loudspeaker plays and
//! which is up to 2.6 dB above the largest sample for these sounds. Every level is clamped
//! to −6 dBFS. The mixer's master gain multiplies on top: keep it at or below 1.

use std::collections::VecDeque;
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU32, Ordering};

use guitarness_dsp::OnsetEvent;
use guitarness_dsp::latency::{
    BiasConfig, CalError, LoopbackConfig, Probe, measure_loopback, player_bias,
};
use guitarness_dsp::synth::Mulberry32;
use serde_json::{Value, json};

use crate::clock::{ClockQuality, ClockSnapshot, ClockStep, StampSource, StepCause};
use crate::engine::{EngineEvent, EngineIo, MixerHandle, OnsetNote, TapHandle};
use crate::mixer::{MixCmd, MixEvent};
use crate::sounds::{SoundBank, SoundId};
use crate::stats::Ring;
use crate::types::{OutFrame, Side, StreamInfo};

/// No sound of the calibration plays above this level.
pub const MAX_DBFS: f64 = -6.0;

const TAG_BEAT: u32 = 1;
const TAG_CHECK: u32 = 2;
const TAG_ROUND0: u32 = 16;
/// Ping tags: every calibrator of the process takes the next of `PING_TAGS` tags, so the
/// answer to a ping of an earlier calibrator (still in the event queue) is not taken for
/// an answer to this one.
const TAG_PING0: u32 = 32;
const PING_TAGS: u32 = 32;
static INSTANCES: AtomicU32 = AtomicU32::new(0);
/// A ping is sent this far behind the last known frame: the answer is always `Late` with
/// a small `by`, and the sound is over before it could take a voice.
const PING_BACK: u64 = 1 << 20;
/// Steps without an answer before a ping is sent again (a lost event must not stall).
const PING_RESEND_STEPS: u32 = 64;
/// Input time without an answer before a ping is sent again, seconds: a driver that steps
/// slowly must not take one lost answer for a silent mixer.
const PING_RESEND_S: f64 = 0.5;
/// Input time without any answer of the mixer after which the calibration ends, seconds
/// (the engine runs, the commands do not arrive: dead handles, streams gone).
const MIXER_DEAD_S: f64 = 4.0;
/// A `Stopped` read before the mixer answered may be old news of streams that were
/// reopened since: it waits this many steps for the `Reopened` that says so.
const STOP_WAIT_STEPS: u32 = 8;
/// A command is given up when its start frame is nearer than this (or two answer
/// intervals), seconds: it could no longer reach the mixer in time.
const SEND_MARGIN_S: f64 = 0.03;
/// Times the check probe of an all-zero input is planned again when the mixer queue did
/// not take it in time.
const CHECK_TRIES: u32 = 3;
/// Samples read from the tap per call.
const SCRATCH: usize = 8192;
/// A sample at or above this magnitude counts as clipped.
const CLIP_LEVEL: f32 = 0.985;
/// Clipped fraction of the silence window that asks the player to move away.
const CLIP_FRACTION: f64 = 1.0e-3;
/// Length of the probe (`guitarness_dsp::latency::Probe`), seconds.
const PROBE_S: f64 = 0.020;
/// Shortest schedule lead, seconds.
const LEAD_S: f64 = 0.12;
/// The tap stays off this long, and (`TAP_QUIET_S`) the input clock must advance this far
/// without tap data, before it is switched on again: the capture stage has then seen
/// "off", so the next origin is a fresh one.
const TAP_OFF_S: f64 = 0.25;
const TAP_QUIET_S: f64 = 0.15;
const TAP_OFF_MAX_S: f64 = 2.0;
/// The first tap sample cannot be older than this before the tap was switched on.
const TAP_ORIGIN_SLACK_S: f64 = 0.1;
/// Times the tap may be reset because its origin was stale.
const TAP_RESETS: u32 = 2;
/// Input time that passes between two steps when the driver stalled: the frame counter
/// read before it is stale, so the machine waits for a fresh one.
const STALL_S: f64 = 0.3;
/// Step 0 waits this long (since the start) for the clock to lock.
const QUICK_WAIT_S: f64 = 4.5;
/// After the silence, the clock gets this long to lock.
const LOCK_WAIT_S: f64 = 2.5;
/// After a stamp fallback, or when it lost its lock under a probe round, the clock
/// re-acquires (one lock window of 2 s): it gets this long to lock again.
const RELOCK_WAIT_S: f64 = 5.0;
/// The detector reports a strum this long after it happened, at the latest, seconds.
const STRUM_REPORT_S: f64 = 0.2;
/// Shortest silence measured after a failed quick check.
const SILENCE_MIN_S: f64 = 1.0;
/// Silence measured again after [`Prompt::TooLoud`].
const LOUD_RETRY_S: f64 = 1.5;
/// How long after the check probe the input may still be digital silence.
const BLOCK_CHECK_S: f64 = 0.7;
/// Pause before a round is repeated after a glitch.
const GLITCH_PAUSE_S: f64 = 0.3;
/// A round is analysed this long after its last window at the latest.
const ROUND_GRACE_S: f64 = 1.0;
/// Snapshots and span (seconds) a fitted line needs; otherwise the last snapshot is used.
const FIT_MIN_POINTS: usize = 8;
const FIT_MIN_SPAN_S: f64 = 1.0;

// ------------------------------------------------------------------ profile

/// What a profile belongs to: the host API, both devices and both rates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileKey {
    /// Host API name (for example "CoreAudio", "WASAPI", "fake").
    pub host: String,
    /// Input device id.
    pub in_id: String,
    /// Output device id.
    pub out_id: String,
    /// Input rate, Hz.
    pub fs_in: u32,
    /// Output rate, Hz.
    pub fs_out: u32,
}

impl ProfileKey {
    /// The key of the open streams.
    pub fn new(host: impl Into<String>, info: &StreamInfo) -> Self {
        Self {
            host: host.into(),
            in_id: info.in_id.clone(),
            out_id: info.out_id.clone(),
            fs_in: info.fs_in,
            fs_out: info.fs_out,
        }
    }
}

/// What a judge may subtract from `t_onset − in_of_out(f_beat)`: see [`Profile::basis`]
/// and the module documentation ("System and player").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// The acoustic loop was measured: subtract `L_rt` and nothing else. What is left is
    /// the player's, and a tendency may be claimed from it.
    Loopback,
    /// No loop: the strums gave latency plus tendency as one number. Timing may be judged
    /// against it; a tendency may never be claimed.
    Estimate,
    /// Nothing to judge timing with: the rhythm is shown, not judged.
    None,
}

/// When step 5 (muted strums over the click) runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BeatStep {
    /// Only when the loop could not be measured: the strums are then the only offset
    /// there is. With a loop the calibration ends after the probes.
    #[default]
    WhenNoLoop,
    /// Always. With a loop, what the strums give is recorded as an observation
    /// ([`Profile::tendency_ms`]) and changes nothing else.
    Always,
    /// Never. Without a loop the calibration then ends without a basis
    /// ([`Prompt::RhythmLater`]).
    Never,
}

/// The result of a calibration for one device pair (plan §4 step 6).
///
/// Judging uses `e = (t_onset − in_of_out(f_beat)) − off`, with `off` =
/// [`Profile::judge_offset_ms`]: the latency of the system with a loop, the one number
/// the strums gave without it. The player's tendency is never subtracted.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// Device pair.
    pub key: ProfileKey,
    /// Round-trip latency of the system, ms. `None`: the loop could not be measured
    /// (headphones, Bluetooth).
    pub l_rt_ms: Option<f64>,
    /// MAD of the per-probe latencies, ms.
    pub mad_ms: Option<f64>,
    /// With a loop, when step 5 ran: the median of `strum − click − L_rt`, ms (positive:
    /// strums after the beat). An observation of the player during that step. No judge
    /// subtracts it.
    pub tendency_ms: Option<f64>,
    /// MAD of those strum errors, ms.
    pub tendency_mad_ms: Option<f64>,
    /// Without a loop: the median of `strum − click`, ms. Latency and tendency in one
    /// number (addendum A3); nothing can tell them apart.
    pub combined_ms: Option<f64>,
    /// MAD of those strum offsets, ms.
    pub combined_mad_ms: Option<f64>,
    /// Strums behind `combined_ms`.
    pub combined_n: Option<u32>,
    /// Timing may be judged with this profile. With a loop: the latency is valid (it
    /// passed a probe round on a clock map that stayed locked, and the map was locked
    /// at the end), whatever the strums. Without: the strums are consistent (MAD
    /// ≤ 15 ms), their offset is unambiguous and below the ceiling, and the map was
    /// locked at the end. `false`: shown, not judged ("not yet").
    pub judgeable: bool,
    /// Date given by the driver ([`CalibConfig::date`]).
    pub date: String,
    /// Clock map quality when the calibration ended.
    pub clock_quality: ClockQuality,
    /// Input clock against the output clock when the calibration ended, ppm.
    pub drift_ppm: f64,
    /// Stamps that fed the input line of the clock map during the calibration. The
    /// numbers hold only on a map fed the same way: see [`Profile::holds_on`].
    pub stamps_in: StampSource,
    /// Stamps that fed the output line of the clock map during the calibration.
    pub stamps_out: StampSource,
}

fn source_name(s: StampSource) -> &'static str {
    match s {
        StampSource::Device => "device",
        StampSource::Callback => "callback",
    }
}

fn source_from(name: &str) -> Option<StampSource> {
    [StampSource::Device, StampSource::Callback]
        .into_iter()
        .find(|s| source_name(*s) == name)
}

fn quality_name(q: ClockQuality) -> &'static str {
    match q {
        ClockQuality::Acquiring => "acquiring",
        ClockQuality::Locked => "locked",
        ClockQuality::Degraded => "degraded",
        ClockQuality::Broken => "broken",
    }
}

fn quality_from(name: &str) -> Option<ClockQuality> {
    [
        ClockQuality::Acquiring,
        ClockQuality::Locked,
        ClockQuality::Degraded,
        ClockQuality::Broken,
    ]
    .into_iter()
    .find(|q| quality_name(*q) == name)
}

/// One optional measurement of a profile entry.
enum Field {
    /// The key is not there.
    Absent,
    /// `null`: explicitly not measured.
    Null,
    /// A finite number.
    Number(f64),
}

impl Field {
    fn value(&self) -> Option<f64> {
        match self {
            Self::Number(x) => Some(*x),
            _ => None,
        }
    }
}

/// Strums behind the `bias_ms` of a version 1 profile, which did not store their number:
/// the fewest a bias was ever computed from (`BiasConfig::min_strums`, also the early
/// stop of step 5), so the uncertainty derived from it is not understated.
const V1_STRUMS: u32 = 12;

/// Largest `latency + tendency` read without a loop that is judged with, ms: the default
/// of [`CalibConfig::a3_max_offset_ms`].
const A3_MAX_OFFSET_MS: f64 = 500.0;

/// The basis of a profile is within the limits the calibrator checks, with the settings
/// of [`CalibConfig::new`], before it calls a profile judgeable:
///
/// - with a loop (`loop_mad`), its MAD within [`LoopbackConfig::max_mad_ms`];
/// - without (`combined`), the MAD of the strums within [`BiasConfig::judge_max_mad_ms`],
///   at least [`BiasConfig::min_strums`] of them, and the offset up to
///   [`CalibConfig::a3_max_offset_ms`].
///
/// A file can say `judgeable` next to anything: what contradicts these is not trusted (a
/// profile made with looser settings included). The calibration then simply runs again.
fn within_limits(loop_mad: Option<f64>, combined: Option<(f64, f64, u32)>) -> bool {
    let bias = BiasConfig::default();
    match (loop_mad, combined) {
        (Some(mad), None) => mad <= LoopbackConfig::default().max_mad_ms,
        (None, Some((ms, mad, n))) => {
            mad <= bias.judge_max_mad_ms
                && usize::try_from(n).is_ok_and(|n| n >= bias.min_strums)
                && ms <= A3_MAX_OFFSET_MS
        }
        _ => false,
    }
}

/// What the strums of step 5 gave, as one profile entry stores it.
struct StrumFields {
    /// `(tendency_ms, tendency_mad_ms)`.
    tendency: Option<(f64, f64)>,
    /// `(combined_ms, combined_mad_ms, combined_n)`.
    combined: Option<(f64, f64, u32)>,
    judgeable: bool,
}

impl Profile {
    /// What timing may be judged with. [`Basis::None`] unless the profile is
    /// [`judgeable`](Profile::judgeable); otherwise [`Basis::Loopback`] when it has the
    /// latency of the loop and its MAD, [`Basis::Estimate`] when it has `combined_ms`,
    /// its MAD and its count instead. The numbers a basis names are therefore present.
    ///
    /// This is about the profile alone. On a clock map fed by other stamps than the ones
    /// it was made on, no number of it applies: ask [`Profile::holds_on`] first.
    pub fn basis(&self) -> Basis {
        if !self.judgeable {
            Basis::None
        } else if self.l_rt_ms.is_some() && self.mad_ms.is_some() {
            Basis::Loopback
        } else if self.l_rt_ms.is_none()
            && self.combined_ms.is_some()
            && self.combined_mad_ms.is_some()
            && self.combined_n.is_some_and(|n| n > 0)
        {
            Basis::Estimate
        } else {
            Basis::None
        }
    }

    /// What to subtract from `t_onset − in_of_out(f_beat)`, ms: `l_rt_ms` on
    /// [`Basis::Loopback`], `combined_ms` on [`Basis::Estimate`], nothing to judge with
    /// on [`Basis::None`]. Never `L_rt` plus the player's tendency: that sum would make
    /// the player's average the beat.
    pub fn judge_offset_ms(&self) -> Option<f64> {
        match self.basis() {
            Basis::Loopback => self.l_rt_ms,
            Basis::Estimate => self.combined_ms,
            Basis::None => None,
        }
    }

    /// The numbers of the profile apply to the clock map of `snap`: both lines are fed by
    /// the stamps they were fed by during the calibration. `in_of_out` on callback stamps
    /// differs from `in_of_out` on device stamps by a constant, so after a
    /// [`StepCause::Fallback`] (or on the next start, if the fallback does not happen
    /// again) a saved `L_rt` is off by that constant: calibrate again.
    pub fn holds_on(&self, snap: &ClockSnapshot) -> bool {
        snap.source(Side::In) == self.stamps_in && snap.source(Side::Out) == self.stamps_out
    }

    /// The profile as a JSON object (missing numbers are `null`).
    pub fn to_json(&self) -> Value {
        json!({
            "host": self.key.host,
            "in_id": self.key.in_id,
            "out_id": self.key.out_id,
            "fs_in": self.key.fs_in,
            "fs_out": self.key.fs_out,
            "l_rt_ms": self.l_rt_ms,
            "mad_ms": self.mad_ms,
            "tendency_ms": self.tendency_ms,
            "tendency_mad_ms": self.tendency_mad_ms,
            "combined_ms": self.combined_ms,
            "combined_mad_ms": self.combined_mad_ms,
            "combined_n": self.combined_n,
            "judgeable": self.judgeable,
            "date": self.date,
            "clock_quality": quality_name(self.clock_quality),
            "drift_ppm": self.drift_ppm,
            "stamps_in": source_name(self.stamps_in),
            "stamps_out": source_name(self.stamps_out),
        })
    }

    /// Reads [`Profile::to_json`] back, strictly: a profile that cannot be trusted as a
    /// whole is no profile (the calibration then simply runs in full). `None` if
    ///
    /// - a key field or `judgeable` is missing or of the wrong type;
    /// - any other field is present with the wrong type, an unknown name or a number
    ///   that is not finite, or a MAD below zero. A measurement written as text
    ///   (`"combined_ms": "108.2"`) must not read as "not measured" next to
    ///   `judgeable: true`;
    /// - a value is there without its MAD (or `combined_ms` without its count), or the
    ///   other way round;
    /// - the numbers contradict each other: a tendency without the latency it was read
    ///   against, or `combined_ms` next to a latency;
    /// - `judgeable` is `true` without a basis: `l_rt_ms` as a number, or `l_rt_ms` as an
    ///   explicit `null` (no loop, addendum A3) together with `combined_ms`;
    /// - `judgeable` is `true` of numbers the calibrator would not judge with: a clock map
    ///   that was not locked (`clock_quality` absent included), or a basis beyond the
    ///   limits of [`CalibConfig::new`]: a loop MAD above 3 ms, or without a loop a strum
    ///   MAD above 15 ms, fewer than 12 strums or an offset above 500 ms.
    ///
    /// Absent `date`, `clock_quality`, `drift_ppm`, `stamps_in` and `stamps_out` take
    /// their defaults (`""`, acquiring, 0, device, device): files from before these
    /// fields existed still load.
    ///
    /// # Version 1 entries
    ///
    /// An entry with `bias_ms` or `bias_mad_ms` and none of the fields that replaced them
    /// is a version 1 entry. It is read with the rules of version 1 (`judgeable` needs
    /// `bias_ms`, and `l_rt_ms` as a number or an explicit `null`; a judgeable entry has
    /// its numbers within the limits above) and converted:
    ///
    /// - with a latency, the bias was the player's own: it becomes `tendency_ms`, an
    ///   observation. `judgeable` no longer depends on the strums: it is `true` when the
    ///   loop MAD is within its limit and the clock map was locked when the profile was
    ///   made (a version 1 profile on an acquiring map was judgeable; it is not now);
    /// - without a latency, the bias held latency and tendency: it becomes `combined_ms`,
    ///   with a count of 12 (the fewest strums a bias was computed from; the file did not
    ///   store the number), and `judgeable` stays as written if the map was locked.
    ///
    /// An entry with fields of both versions is no profile.
    pub fn from_json(v: &Value) -> Option<Self> {
        let text = |k: &str| v.get(k)?.as_str().map(str::to_owned);
        let rate = |k: &str| u32::try_from(v.get(k)?.as_u64()?).ok();
        // `Err`: present and unusable.
        let field = |k: &str| match v.get(k) {
            None => Ok(Field::Absent),
            Some(Value::Null) => Ok(Field::Null),
            Some(Value::Number(n)) => n
                .as_f64()
                .filter(|x| x.is_finite())
                .map(Field::Number)
                .ok_or(()),
            Some(_) => Err(()),
        };
        // A MAD is a distance: below zero it is no measurement.
        let spread = |k: &str| match field(k)? {
            Field::Number(x) if x < 0.0 => Err(()),
            f => Ok(f),
        };
        let optional_text = |k: &str| match v.get(k) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(()),
        };
        let key = ProfileKey {
            host: text("host")?,
            in_id: text("in_id")?,
            out_id: text("out_id")?,
            fs_in: rate("fs_in")?,
            fs_out: rate("fs_out")?,
        };
        let judgeable = v.get("judgeable")?.as_bool()?;
        let l_rt = field("l_rt_ms").ok()?;
        let mad = spread("mad_ms").ok()?;
        let drift = field("drift_ppm").ok()?;
        if l_rt.value().is_some() != mad.value().is_some() {
            return None;
        }
        let clock_quality = match optional_text("clock_quality").ok()? {
            None => ClockQuality::Acquiring,
            Some(name) => quality_from(name)?,
        };
        let has = |keys: &[&str]| keys.iter().any(|k| v.get(*k).is_some());
        let v1 = has(&["bias_ms", "bias_mad_ms"]);
        let v2 = has(&[
            "tendency_ms",
            "tendency_mad_ms",
            "combined_ms",
            "combined_mad_ms",
            "combined_n",
        ]);
        let strums = match (v1, v2) {
            (true, true) => return None,
            (true, false) => {
                let bias = field("bias_ms").ok()?;
                let bias_mad = spread("bias_mad_ms").ok()?;
                if bias.value().is_some() != bias_mad.value().is_some() {
                    return None;
                }
                if judgeable && (bias.value().is_none() || matches!(l_rt, Field::Absent)) {
                    return None;
                }
                let pair = bias.value().zip(bias_mad.value());
                let locked = clock_quality == ClockQuality::Locked;
                if l_rt.value().is_some() {
                    // Version 1 said "judgeable" of the latency and of the strums.
                    let loop_ok = within_limits(mad.value(), None);
                    let strums_ok =
                        pair.is_some_and(|(_, m)| m <= BiasConfig::default().judge_max_mad_ms);
                    if judgeable && !(loop_ok && strums_ok) {
                        return None;
                    }
                    StrumFields {
                        tendency: pair,
                        combined: None,
                        judgeable: locked && loop_ok,
                    }
                } else {
                    let combined = pair.map(|(ms, mad)| (ms, mad, V1_STRUMS));
                    if judgeable && !within_limits(None, combined) {
                        return None;
                    }
                    StrumFields {
                        tendency: None,
                        combined,
                        judgeable: judgeable && locked,
                    }
                }
            }
            _ => {
                let tendency = field("tendency_ms").ok()?;
                let tendency_mad = spread("tendency_mad_ms").ok()?;
                let combined = field("combined_ms").ok()?;
                let combined_mad = spread("combined_mad_ms").ok()?;
                let combined_n = match v.get("combined_n") {
                    None | Some(Value::Null) => None,
                    Some(n) => Some(u32::try_from(n.as_u64()?).ok().filter(|n| *n > 0)?),
                };
                if tendency.value().is_some() != tendency_mad.value().is_some()
                    || combined.value().is_some() != combined_mad.value().is_some()
                    || combined.value().is_some() != combined_n.is_some()
                {
                    return None;
                }
                // A tendency is read against a latency; `combined_ms` exists because
                // there is none.
                if tendency.value().is_some() && l_rt.value().is_none()
                    || combined.value().is_some() && l_rt.value().is_some()
                {
                    return None;
                }
                let basis = l_rt.value().is_some()
                    || matches!(l_rt, Field::Null) && combined.value().is_some();
                let combined = combined
                    .value()
                    .zip(combined_mad.value())
                    .zip(combined_n)
                    .map(|((ms, mad), n)| (ms, mad, n));
                // The calibrator says "judgeable" only of numbers within its limits, on
                // a locked clock map.
                if judgeable
                    && !(basis
                        && clock_quality == ClockQuality::Locked
                        && within_limits(mad.value(), combined))
                {
                    return None;
                }
                StrumFields {
                    tendency: tendency.value().zip(tendency_mad.value()),
                    combined,
                    judgeable,
                }
            }
        };
        let source = |k: &str| match optional_text(k) {
            Ok(None) => Some(StampSource::Device),
            Ok(Some(name)) => source_from(name),
            Err(()) => None,
        };
        Some(Self {
            key,
            l_rt_ms: l_rt.value(),
            mad_ms: mad.value(),
            tendency_ms: strums.tendency.map(|t| t.0),
            tendency_mad_ms: strums.tendency.map(|t| t.1),
            combined_ms: strums.combined.map(|c| c.0),
            combined_mad_ms: strums.combined.map(|c| c.1),
            combined_n: strums.combined.map(|c| c.2),
            judgeable: strums.judgeable,
            date: optional_text("date").ok()?.unwrap_or_default().to_owned(),
            clock_quality,
            drift_ppm: match drift {
                Field::Number(x) => x,
                Field::Absent => 0.0,
                Field::Null => return None,
            },
            stamps_in: source("stamps_in")?,
            stamps_out: source("stamps_out")?,
        })
    }
}

/// Version of the profile file that [`profiles_to_json`] writes.
const PROFILE_VERSION: u64 = 2;

/// The profile file: `{"version": 2, "profiles": [...]}`, one entry per device pair.
pub fn profiles_to_json(profiles: &[Profile]) -> String {
    let list: Vec<Value> = profiles.iter().map(Profile::to_json).collect();
    let doc = json!({ "version": PROFILE_VERSION, "profiles": list });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| doc.to_string())
}

/// Reads [`profiles_to_json`], and the file of version 1 (its entries are converted: see
/// [`Profile::from_json`]). Text that is not such a file gives no profiles, and an entry
/// that does not parse is skipped: the calibration then simply runs in full.
pub fn profiles_from_json(text: &str) -> Vec<Profile> {
    let Ok(doc) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    if !matches!(
        doc.get("version").and_then(Value::as_u64),
        Some(1 | PROFILE_VERSION)
    ) {
        return Vec::new();
    }
    doc.get("profiles")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Profile::from_json).collect())
        .unwrap_or_default()
}

/// The profile of `key`, if any.
pub fn find_profile<'a>(profiles: &'a [Profile], key: &ProfileKey) -> Option<&'a Profile> {
    profiles.iter().find(|p| &p.key == key)
}

/// Replaces the profile with the same key, or adds `profile`.
pub fn upsert_profile(profiles: &mut Vec<Profile>, profile: Profile) {
    match profiles.iter_mut().find(|p| p.key == profile.key) {
        Some(slot) => *slot = profile,
        None => profiles.push(profile),
    }
}

// ------------------------------------------------------------------ public types

/// Settings of a calibration. [`CalibConfig::new`] is the M2 plan with the full probe
/// round; [`CalibConfig::short`] puts the short round in front of it.
#[derive(Clone, Debug)]
pub struct CalibConfig {
    /// Device pair of the open streams (also gives the two rates).
    pub key: ProfileKey,
    /// Date written into the profile. The state machine reads no clock.
    pub date: String,
    /// Seed of the probe spacing.
    pub seed: u32,
    /// Step 1: seconds of silence (the tap reset at the start counts). At least 1 s is
    /// measured after the tap reset.
    pub silence_s: f64,
    /// When step 5 runs.
    pub beat_step: BeatStep,
    /// Probes of the short round that runs before the full one. 0: no short round.
    pub short_probes: u32,
    /// Short round: mean time between probes, seconds.
    pub short_gap_s: f64,
    /// Short round: the gaps are spread evenly over ± this, seconds, in a random order
    /// (see "The short round" in the module documentation). The step between two of
    /// them must stay well above the MAD limit of [`CalibConfig::loopback`].
    pub short_gap_jitter_s: f64,
    /// Short round: probes that must be heard and vouched for (see "The short round" in
    /// the module documentation). Fewer, and the full round runs.
    pub short_min_found: u32,
    /// Step 2: probes per round.
    pub probes: u32,
    /// Step 0: probes of the quick check.
    pub quick_probes: u32,
    /// Mean time between probes, seconds.
    pub probe_gap_s: f64,
    /// The gap varies by ± this, seconds (uniform).
    pub probe_gap_jitter_s: f64,
    /// Level of the first round and of the quick check, dBFS.
    pub probe_dbfs: f64,
    /// Level of the second round, dBFS.
    pub retry_dbfs: f64,
    /// Level of the click, dBFS (the accent is the same gain on a louder sound; the gain
    /// is lowered if the accent would pass [`MAX_DBFS`]).
    pub click_dbfs: f64,
    /// Step 0: the profile is kept when the latency moved by at most this, ms.
    pub quick_tolerance_ms: f64,
    /// Step 5: tempo.
    pub bpm: f64,
    /// Step 5: clicks before the first strum.
    pub count_in: u32,
    /// Step 5: beats to strum on.
    pub beats: u32,
    /// Step 5: stop as soon as this many strums are paired and consistent (MAD within
    /// the limit of [`CalibConfig::bias`]).
    pub early_stop_pairs: usize,
    /// The search for a probe starts this long before its stamped emission, seconds.
    pub pre_roll_s: f64,
    /// Fitted line against the live clock map: above this, ms, a warning is recorded.
    pub fit_warn_ms: f64,
    /// Matched-filter limits (`search_max_s` counts from the emission).
    pub loopback: LoopbackConfig,
    /// Pairing limits of the strums of step 5, and the MAD up to which an offset read
    /// without a loop is judged with.
    pub bias: BiasConfig,
    /// After a kept profile without a loop that is not judgeable, run step 5 again ("try
    /// again later"). Not with [`BeatStep::Never`].
    pub retry_rhythm: bool,
    /// Without a loop (addendum A3): largest `latency + tendency` that is accepted, ms. A
    /// larger offset ends "not judged yet", like scattered strums.
    ///
    /// 500 ms: a Bluetooth output delays the click by 150 to 300 ms, the input adds 10 to
    /// 50 ms and a player's own tendency stays within ±50 ms, so real setups sit below
    /// 400 ms.
    /// The offset is read on the beat grid, in `[−0.15, 0.85)` beat (637 ms at 80 BPM): the
    /// ceiling must stay well below that edge, where a reading turns into its alias one
    /// beat away. Above 500 ms the player also hears every click more than half a beat
    /// late, and a beat kept against that is not a rhythm to judge.
    pub a3_max_offset_ms: f64,
    /// [`Calibrator::step`] calls without any answer from the mixer after which the
    /// calibration ends with [`Outcome::Interrupted`], so that no state waits for ever.
    ///
    /// This is the last resort, for an engine that says nothing at all: the calibrator
    /// has no clock of its own, so it can only count its steps. An engine whose analysis
    /// runs is timed instead: 4 s of input without an answer end the calibration whatever
    /// this value. 3000: 15 s at 5 ms per step, 50 s at 60 steps per second. A driver
    /// that steps more often than every 2 ms should raise it (a stalled driver gives no
    /// answer either, and a stall of a second or two is survived otherwise).
    pub max_silent_steps: u32,
    /// Tags `tag_base..tag_base + 64` are used for the mixer commands.
    pub tag_base: u32,
}

impl CalibConfig {
    /// The plan's values for the device pair `key`; `date` goes into the profile.
    pub fn new(key: ProfileKey, date: impl Into<String>) -> Self {
        Self {
            key,
            date: date.into(),
            seed: 0x7061_6C6D,
            silence_s: 3.0,
            beat_step: BeatStep::WhenNoLoop,
            short_probes: 0,
            short_gap_s: 0.30,
            short_gap_jitter_s: 0.04,
            short_min_found: 5,
            probes: 10,
            quick_probes: 3,
            probe_gap_s: 0.45,
            probe_gap_jitter_s: 0.04,
            probe_dbfs: -12.0,
            retry_dbfs: -6.0,
            click_dbfs: -8.0,
            quick_tolerance_ms: 2.0,
            bpm: 80.0,
            count_in: 4,
            beats: 16,
            early_stop_pairs: 12,
            pre_roll_s: 0.05,
            fit_warn_ms: 0.3,
            loopback: LoopbackConfig::default(),
            bias: BiasConfig::default(),
            retry_rhythm: false,
            a3_max_offset_ms: A3_MAX_OFFSET_MS,
            max_silent_steps: 3000,
            tag_base: 0xCA11_B000,
        }
    }

    /// The short check (Pulso plan §2.2), for where an acoustic loop exists: 1 s of
    /// silence, then 6 probes 300 ± 40 ms apart, of which 5 must be heard. About 3.4 s
    /// on a locked clock, and the player does nothing. A refused short round is not an
    /// ending: the full round of [`CalibConfig::new`] follows, then the louder one, then
    /// the strums.
    pub fn short(key: ProfileKey, date: impl Into<String>) -> Self {
        Self {
            silence_s: 1.0,
            short_probes: 6,
            ..Self::new(key, date)
        }
    }
}

/// What the UI says now. Short text, no numbers; the counts are for a countdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// Step 0: checking the saved setup with a few beeps. Nothing to do.
    Checking,
    /// Step 1: stay quiet for a moment.
    Quiet,
    /// The input is clipping: move away from the loudspeaker or lower the input level.
    /// The silence is measured once more.
    TooLoud,
    /// Step 2: `count` beeps will play. No need to play anything.
    WillBeep {
        /// Beeps in this round.
        count: u8,
    },
    /// Step 4: the beeps were not heard; `count` more, louder.
    WillBeepLouder {
        /// Beeps in this round.
        count: u8,
    },
    /// Step 5: mute the strings and strum on every click. Never shown when the loop was
    /// measured and [`CalibConfig::beat_step`] is [`BeatStep::WhenNoLoop`], or with
    /// [`BeatStep::Never`].
    StrumMuted {
        /// Clicks left before the first strum (0 once the strums count).
        count_in: u8,
        /// Beat being strummed, 1-based (0 during the count-in).
        beat: u8,
        /// Beats in all.
        of: u8,
    },
    /// Finished: the game is set up and rhythm is judged ([`Profile::basis`] says with
    /// what).
    Done,
    /// Finished without a verdict on rhythm: it is shown but not judged yet. Offer to
    /// try again later. Not a failure.
    RhythmLater,
    /// The microphone delivers only silence: it is blocked or muted. Stopped.
    MicBlocked,
    /// The audio device changed or stopped. Stopped; reopen and calibrate again.
    Interrupted,
}

/// What [`Calibrator::step`] returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibUpdate {
    /// What to show.
    pub prompt: Prompt,
    /// The prompt differs from the previous update (always `true` on the first one).
    pub changed: bool,
    /// The calibration ended: read [`Calibrator::outcome`].
    pub done: bool,
}

/// How a calibration ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The saved profile still holds (quick check). Clear the engine's stale flag.
    Kept(Profile),
    /// A new profile. Save it and clear the engine's stale flag. `l_rt_ms` may be `None`
    /// (addendum A3) and `judgeable` may be `false`: [`Profile::basis`] says what can be
    /// judged with.
    Calibrated(Profile),
    /// The clock map never locked, or changed its stamp source more often than a
    /// measurement can be repeated: nothing was measured. Do not judge rhythm, try again
    /// later. A saved profile is neither confirmed nor replaced: use it only if
    /// [`Profile::holds_on`] the clock map.
    Unstable,
    /// The input is digital silence.
    MicBlocked,
    /// Device event, or [`Calibrator::cancel`].
    Interrupted,
}

impl Outcome {
    /// The profile to use, if the calibration produced or kept one.
    pub fn profile(&self) -> Option<&Profile> {
        match self {
            Self::Kept(p) | Self::Calibrated(p) => Some(p),
            _ => None,
        }
    }
}

/// Which probe round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundKind {
    /// Step 0: the quick check of a saved profile.
    Quick,
    /// The short round of [`CalibConfig::short`], before the full one.
    Short,
    /// Step 2: the first full round.
    First,
    /// Step 4: the second round, louder.
    Retry,
}

/// Numbers of one probe round (`--detalhes`).
#[derive(Clone, Debug, PartialEq)]
pub struct RoundDetails {
    /// Which round.
    pub kind: RoundKind,
    /// Probe level, dBFS.
    pub level_dbfs: f64,
    /// Probes the mixer queue took in time.
    pub sent: u32,
    /// Probes that started on time (`Started`); only these are measured.
    pub started: u32,
    /// Probes the mixer reported `Late` (discarded).
    pub late: u32,
    /// Probes heard, when known.
    pub found: Option<u32>,
    /// Median latency, ms.
    pub l_rt_ms: Option<f64>,
    /// MAD of the latencies, ms.
    pub mad_ms: Option<f64>,
    /// Latency of each started probe, ms (`None`: not heard).
    pub per_probe_ms: Vec<Option<f64>>,
    /// Why the round gave no latency.
    pub refused: Option<CalError>,
    /// Largest difference between the fitted line and the live clock map, ms.
    pub fit_vs_live_ms: Option<f64>,
    /// The emission times come from the fitted line (`false`: from the last snapshot).
    pub fitted: bool,
    /// A clock step or a tap overflow hit the round: it was not analysed.
    pub glitch: bool,
}

/// Numbers of the quick check (`--detalhes`).
#[derive(Clone, Debug, PartialEq)]
pub struct QuickCheck {
    /// `L_rt` of the saved profile, ms.
    pub profile_l_rt_ms: Option<f64>,
    /// `L_rt` measured now, ms.
    pub measured_ms: Option<f64>,
    /// Measured minus saved, ms.
    pub delta_ms: Option<f64>,
    /// The profile was kept.
    pub kept: bool,
}

/// Things worth a line in the report. None of them stops the calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Warning {
    /// The fitted line and the live clock map differ by more than
    /// [`CalibConfig::fit_warn_ms`].
    ClockFitMismatch,
    /// The input still clipped after [`Prompt::TooLoud`].
    InputClipped,
    /// A probe round was hit by a clock step or a tap overflow.
    RoundGlitch,
    /// The mixer took a command after its start frame.
    LateCommand,
    /// No `Pitch` or `Onset` event gave the detector-to-input-frame offset, or no tap
    /// sample arrived: the loop was not measured.
    NoInputClock,
    /// The capture stage was not seen idle before the tap was switched on.
    TapNotIdle,
    /// The metronome was refused by the mixer, or its queue did not take it in time.
    NoMetronome,
    /// The clock map changed its stamp source ([`StepCause::Fallback`]): what was being
    /// measured, and a latency measured before, were on a map that is gone. They were
    /// measured again (once) or given up.
    StampFallback,
    /// Step 5 was hit by a clock step whose unjudged window never came: measured again.
    BeatGlitch,
    /// The saved profile was made on other stamps than the clock map uses now
    /// ([`Profile::holds_on`]): not kept.
    ProfileOtherStamps,
    /// Without a loop: the strums fit the clicks one or more beats earlier at least as
    /// well (a late start, or a latency of most of a beat). The offset is not known:
    /// rhythm is not judged.
    OffsetUnresolved,
    /// Without a loop: the offset is above [`CalibConfig::a3_max_offset_ms`]. Rhythm is
    /// not judged.
    OffsetBeyondCeiling,
    /// The mixer queue was full: a sound was not sent in time and was left out.
    QueueFull,
    /// The mixer did not answer (for 4 s of input, or [`CalibConfig::max_silent_steps`]
    /// steps), or its handle refused a command with room in the queue (streams reopened
    /// or dropped under the calibration): ended as interrupted.
    NoMixer,
}

/// Everything measured, in ms and dBFS (`--detalhes`).
#[derive(Clone, Debug, PartialEq)]
pub struct Details {
    /// RMS of the silence, dBFS. `None`: digital silence, or not measured.
    pub noise_floor_dbfs: Option<f64>,
    /// Peak of the silence, dBFS.
    pub input_peak_dbfs: Option<f64>,
    /// The silence clipped at least once.
    pub clipped: bool,
    /// Step 0, if a saved profile was checked.
    pub quick: Option<QuickCheck>,
    /// Every probe round, in order.
    pub rounds: Vec<RoundDetails>,
    /// `L_rt` in use, ms.
    pub l_rt_ms: Option<f64>,
    /// Its MAD, ms.
    pub l_rt_mad_ms: Option<f64>,
    /// The loop was refused: only the strums can give an offset (addendum A3).
    pub no_loop: bool,
    /// Beats played after the count-in.
    pub beats_played: u32,
    /// Strums the detector reported during step 5.
    pub strums_heard: u32,
    /// Strums paired with a beat.
    pub strums_used: u32,
    /// With a loop: median strum error after `L_rt`, ms ([`Profile::tendency_ms`]).
    pub tendency_ms: Option<f64>,
    /// MAD of those strum errors, ms.
    pub tendency_mad_ms: Option<f64>,
    /// Without a loop: median of `strum − click`, ms ([`Profile::combined_ms`]).
    pub combined_ms: Option<f64>,
    /// MAD of those strum offsets, ms.
    pub combined_mad_ms: Option<f64>,
    /// Without a loop: the offset that was read and not accepted
    /// ([`Warning::OffsetUnresolved`], [`Warning::OffsetBeyondCeiling`]), ms.
    pub unresolved_offset_ms: Option<f64>,
    /// Rhythm may be judged.
    pub judgeable: bool,
    /// What it may be judged with ([`Profile::basis`] of the profile kept or made).
    pub basis: Basis,
    /// Clock quality at the end.
    pub clock_quality: ClockQuality,
    /// Input against output clock at the end, ppm.
    pub drift_ppm: f64,
    /// Stamps feeding the input line of the clock map at the end.
    pub stamps_in: StampSource,
    /// Stamps feeding the output line of the clock map at the end.
    pub stamps_out: StampSource,
    /// Notes for the report.
    pub warnings: Vec<Warning>,
    /// Output time from the first to the last step, seconds.
    pub seconds: f64,
}

/// The click schedule of step 5, in output frames: for the UI's beat display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatPlan {
    /// Frame of the first count-in click.
    pub start: OutFrame,
    /// Frames per beat (fractional).
    pub period: f64,
    /// Clicks before the first strum.
    pub count_in: u32,
    /// Beats to strum on.
    pub beats: u32,
}

impl BeatPlan {
    /// Frame of click `k`, counted from the first count-in click (the mixer's rule).
    pub fn click_frame(&self, k: u32) -> OutFrame {
        self.start + (f64::from(k) * self.period).round() as u64
    }

    /// Frame of the click of strummed beat `i` (0-based).
    pub fn strum_frame(&self, i: u32) -> OutFrame {
        self.click_frame(self.count_in + i)
    }
}

// ------------------------------------------------------------------ helpers

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

fn median_mad(v: &[f64]) -> (f64, f64) {
    let mut a = v.to_vec();
    let m = median(&mut a);
    let mut d: Vec<f64> = v.iter().map(|x| (x - m).abs()).collect();
    (m, median(&mut d))
}

/// Play gain that puts a sound of peak `peak` (see [`true_peak`]) at `dbfs`, never above
/// [`MAX_DBFS`] (a hair below it, so that rounding cannot pass it).
fn level_gain(dbfs: f64, peak: f32) -> f32 {
    if peak <= 0.0 || !peak.is_finite() || !dbfs.is_finite() {
        return 0.0;
    }
    let target = 10f64.powf(dbfs.min(MAX_DBFS) / 20.0) * (1.0 - 1.0e-6);
    (target / f64::from(peak)) as f32
}

fn peak_of(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, x| m.max(x.abs()))
}

/// Largest magnitude of the band-limited signal through `samples`, looked for on a grid
/// eight times finer (Hann-windowed sinc). The loudspeaker plays that signal, and between
/// two samples it can be louder than both: the levels are set against this peak.
///
/// A peak can still fall between two points of the fine grid. For a component at the
/// Nyquist frequency the grid reads at least `cos(π/16)` of it, so the value found
/// between the samples is divided by that: the result is never under the real peak.
fn true_peak(samples: &[f32]) -> f32 {
    const OVER: usize = 8;
    const HALF: isize = 24;
    let n = samples.len() as isize;
    let mut between = 0.0f64;
    for i in -1..n {
        for k in 1..OVER {
            let t = i as f64 + k as f64 / OVER as f64;
            let mut acc = 0.0f64;
            for j in (i - HALF + 1).max(0)..(i + HALF + 1).min(n) {
                let x = t - j as f64;
                let window = 0.5 + 0.5 * (PI * x / HALF as f64).cos();
                acc += f64::from(samples[j as usize]) * window * (PI * x).sin() / (PI * x);
            }
            between = between.max(acc.abs());
        }
    }
    let between = between / (PI / (2 * OVER) as f64).cos();
    peak_of(samples).max(between as f32)
}

/// Rounds to `1/scale`: the profile's numbers are short decimals, so the JSON file reads
/// back to exactly the same values.
fn rounded(x: f64, scale: f64) -> f64 {
    (x * scale).round() / scale
}

/// `g = g0 + slope·(f − f0)`: input frame as a function of output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Line {
    f0: f64,
    g0: f64,
    slope: f64,
}

impl Line {
    fn at(&self, f: f64) -> f64 {
        self.g0 + self.slope * (f - self.f0)
    }

    /// The line of one snapshot (`in_of_out` is linear).
    fn of_snapshot(s: &ClockSnapshot, f0: f64) -> Self {
        let g0 = s.in_of_out(f0);
        Self {
            f0,
            g0,
            slope: (s.in_of_out(f0 + 1.0e6) - g0) / 1.0e6,
        }
    }
}

fn least_squares(pts: &[(f64, f64)]) -> Option<Line> {
    let n = pts.len() as f64;
    if pts.len() < 2 {
        return None;
    }
    let f0 = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let g0 = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = pts.iter().map(|p| (p.0 - f0) * (p.0 - f0)).sum();
    let sxy: f64 = pts.iter().map(|p| (p.0 - f0) * (p.1 - g0)).sum();
    (sxx > 0.0).then(|| Line {
        f0,
        g0,
        slope: sxy / sxx,
    })
}

/// Least squares, then again without the points whose residual is more than 3·MAD from
/// the median residual.
fn robust_line(pts: &[(f64, f64)]) -> Option<Line> {
    let first = least_squares(pts)?;
    let res: Vec<f64> = pts.iter().map(|p| p.1 - first.at(p.0)).collect();
    let (med, mad) = median_mad(&res);
    let cut = (3.0 * mad).max(1.0e-9);
    let kept: Vec<(f64, f64)> = pts
        .iter()
        .zip(&res)
        .filter(|(_, r)| (**r - med).abs() <= cut)
        .map(|(p, _)| *p)
        .collect();
    least_squares(&kept).or(Some(first))
}

/// The `n` gaps of a short round, seconds: `mean` plus the `n` values spread evenly over
/// `± jitter`, each used once, in a random order.
///
/// Probes this close have overlapping search windows, so a window can hold a neighbour
/// of its probe, and the reading is then off by the gap between the two. With distinct
/// gaps such readings never agree with each other: the MAD rule sees them.
fn spread_gaps(n: usize, mean: f64, jitter: f64, rng: &mut Mulberry32) -> Vec<f64> {
    let mut gaps: Vec<f64> = (0..n)
        .map(|j| {
            let turn = if n > 1 {
                2.0 * j as f64 / (n - 1) as f64 - 1.0
            } else {
                0.0
            };
            mean + turn * jitter
        })
        .collect();
    for i in (1..n).rev() {
        let j = ((rng.next_f64() * (i + 1) as f64) as usize).min(i);
        gaps.swap(i, j);
    }
    gaps
}

/// The readings of a short round that are known to be of the probe they were searched
/// for, counted from the first probe.
///
/// The probes are one sound, so a window cannot tell whose arrival it holds, and with
/// probes 0.3 s apart and 0.55 s searched the arrival of a neighbour is within reach.
/// `lags[k]` is what window `k` read, in frames after its start `starts[k]` (`None`:
/// nothing). No probe arrives before the start of its own window: that is what the
/// pre-roll is for. Let `g` be the shortest time between two starts. Then, from the
/// first probe on:
///
/// - a reading below `g` is not a *later* probe, which arrives `g` or more after this
///   window starts;
/// - it is not an *earlier* probe either, if every earlier probe was read below `g`
///   as its own arrival: that arrival is then before this window starts. The first
///   probe has none before it.
///
/// So the leading probes that read what the first one read, within `tol` and below `g`
/// with `tol` to spare, were each read as themselves, whatever the latency did from one
/// probe to the next. The first reading that disagrees or is missing ends the count:
/// that probe may have arrived anywhere, in the window of a later one too, and from
/// there on a reading is no longer known to be its own.
fn vouched(lags: &[Option<f64>], starts: &[f64], tol: f64) -> Vec<f64> {
    let gap = starts
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(f64::INFINITY, f64::min);
    let Some(Some(first)) = lags.first().copied() else {
        return Vec::new();
    };
    let in_reach = first + 2.0 * tol < gap;
    if lags.len() != starts.len() || !in_reach {
        return Vec::new();
    }
    lags.iter()
        .map_while(|x| x.filter(|x| (x - first).abs() <= tol))
        .collect()
}

/// Without `L_rt` the whole latency sits in `onset − beat` and can exceed the pairing
/// window of `player_bias`. Coarse value of it: the circular mean of the onset phases on
/// the beat grid, read in `[−0.15, 0.85)` beat (a strum answers its own click; it may
/// anticipate it a little, it cannot answer the next one).
fn coarse_offset(onsets: &[f64], beats: &[f64]) -> f64 {
    if beats.len() < 2 {
        return 0.0;
    }
    let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let period = median(&mut gaps);
    if period <= 0.0 || !period.is_finite() {
        return 0.0;
    }
    let lo = beats[0] - 0.15 * period;
    let hi = beats[beats.len() - 1] + 0.85 * period;
    let (mut s, mut c, mut n) = (0.0f64, 0.0f64, 0u32);
    for &o in onsets.iter().filter(|o| **o >= lo && **o < hi) {
        let phase = 2.0 * PI * (o - beats[0]) / period;
        s += phase.sin();
        c += phase.cos();
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    let mut turn = s.atan2(c) / (2.0 * PI);
    if turn < -0.15 {
        turn += 1.0;
    }
    turn * period
}

/// How badly the strums fit the hypothesis that each comes `offset` after its beat: the
/// strums that answer no beat (none within `window` of `beat + offset`, or one that
/// already has its strum), plus the beats left without a strum although their time has
/// passed (`beat + offset + window <= seen_until`).
///
/// Strums and misses in the middle cost every hypothesis the same. The ends tell them
/// apart: one beat later, the first strum has no beat and the last beat has no strum.
fn alias_cost(onsets: &[f64], beats: &[f64], offset: f64, window: f64, seen_until: f64) -> u32 {
    let mut hit = vec![false; beats.len()];
    let mut cost = 0u32;
    for &o in onsets {
        let nearest = beats
            .iter()
            .enumerate()
            .map(|(i, b)| (i, (o - (b + offset)).abs()))
            .min_by(|x, y| x.1.total_cmp(&y.1));
        match nearest {
            Some((i, d)) if d <= window && !hit[i] => hit[i] = true,
            _ => cost += 1,
        }
    }
    for (b, hit) in beats.iter().zip(&hit) {
        if !hit && b + offset + window <= seen_until {
            cost += 1;
        }
    }
    cost
}

/// Running level of the input.
#[derive(Clone, Copy, Debug, Default)]
struct Level {
    n: u64,
    nonzero: u64,
    clipped: u64,
    sum_sq: f64,
    peak: f32,
}

impl Level {
    fn add(&mut self, x: &[f32]) {
        for &v in x {
            let a = v.abs();
            self.n += 1;
            self.nonzero += u64::from(a > 0.0);
            self.clipped += u64::from(a >= CLIP_LEVEL);
            self.sum_sq += f64::from(v) * f64::from(v);
            self.peak = self.peak.max(a);
        }
    }

    fn floor_dbfs(&self) -> Option<f64> {
        (self.nonzero > 0).then(|| 10.0 * (self.sum_sq / self.n as f64).log10())
    }

    fn peak_dbfs(&self) -> Option<f64> {
        (self.peak > 0.0).then(|| 20.0 * f64::from(self.peak).log10())
    }

    fn clip_fraction(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.clipped as f64 / self.n as f64
        }
    }
}

// ------------------------------------------------------------------ state

#[derive(Clone, Copy, Debug, PartialEq)]
enum Next {
    Silence,
    Round(RoundKind),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Waiting for the first answer of the mixer (the output frame counter).
    Boot,
    /// The tap is off until the capture stage has seen it off.
    TapOff {
        since: OutFrame,
        next: Next,
    },
    /// Step 1.
    Silence {
        until: OutFrame,
    },
    /// All-zero input: one probe decides between a blocked microphone and a silent path.
    BlockedCheck {
        until: OutFrame,
    },
    /// Waiting for the clock lock, the first tap sample and the detector offset.
    Ready {
        from: OutFrame,
        until: OutFrame,
        kind: RoundKind,
    },
    /// Steps 0, 2 and 4: a probe round is playing.
    Probing,
    /// Step 5.
    Beat,
    /// Step 5 without a loop is void (stamp fallback): waiting for the clock to lock
    /// again before it restarts.
    BeatWait {
        from: OutFrame,
        until: OutFrame,
    },
    /// Step 6 on a clock map that is acquiring again: waiting for the lock before the
    /// profile is made. Unlocked at `until`, the profile is not judgeable.
    Settle {
        until: OutFrame,
    },
    Done,
}

/// What a queued command is for (who hears about its fate).
#[derive(Clone, Copy, Debug, PartialEq)]
enum OutKind {
    /// Probe `k` of the round.
    Probe(usize),
    /// The check probe of an all-zero input.
    Check,
    Metronome,
    Stop,
}

/// A command waiting for a free slot of the mixer queue.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Out {
    cmd: MixCmd,
    /// Past this output frame the command is given up. `None`: never.
    due: Option<OutFrame>,
    what: OutKind,
}

struct Round {
    kind: RoundKind,
    tag: u32,
    level_dbfs: f64,
    /// Planned start of every probe.
    ats: Vec<OutFrame>,
    /// The mixer queue took the command.
    queued: Vec<bool>,
    started: Vec<bool>,
    /// `in_of_out(at)` with the snapshot that was current when the probe played.
    live: Vec<Option<f64>>,
    late: u32,
    rec: Vec<f32>,
    /// Detector time of `rec[0]`.
    rec_det0: u64,
    /// `(output frame, in_of_out of it)` per snapshot received during the round.
    pts: Vec<(f64, f64)>,
    glitch: bool,
    /// Longest latency searched, seconds after the emission.
    search_s: f64,
    deadline: OutFrame,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LoopFix {
    /// `L_rt`, input frames.
    frames: f64,
    /// MAD, input frames.
    mad: f64,
}

struct BeatRun {
    plan: BeatPlan,
    /// `in_of_out` of every strummed beat played so far.
    beats_in: Vec<f64>,
    /// Judged strums since the count-in began, input frames.
    onsets: Vec<f64>,
    dirty: bool,
    refused: bool,
    /// Clock steps whose unjudged window has not arrived yet.
    step_open: u32,
    end_at: OutFrame,
}

/// What the strums of step 5 say, in input frames.
struct BiasEst {
    /// Median of the strum errors. With a loop they are taken after `L_rt`: the player's
    /// tendency. Without, they are `strum − click`: latency plus tendency.
    center: f64,
    mad: f64,
    used: usize,
    /// The strums are consistent (and, without a loop, their offset is accepted).
    judgeable: bool,
    /// Without a loop: why the offset is not accepted.
    doubt: Option<Warning>,
}

/// The calibration state machine. See the module documentation.
pub struct Calibrator {
    cfg: CalibConfig,
    fs_in: f64,
    fs_out: f64,
    saved: Option<Profile>,
    probe_peak: f32,
    click_peak: f32,
    rng: Mulberry32,
    phase: Phase,
    prompt: Prompt,
    fresh: bool,
    // Output frame counter, read from the mixer.
    now: Option<OutFrame>,
    t0: OutFrame,
    gap: u64,
    ping_tag: u32,
    ping_at: OutFrame,
    ping_floor: OutFrame,
    ping_out: bool,
    ping_idle: u32,
    /// `silent_in` when the ping in flight was last sent.
    ping_sent_in: f64,
    /// The mixer handle refused a command although its queue had room: the engine
    /// behind it is gone.
    dead: bool,
    /// Pings sent and the number of the last one answered.
    ping_sent: u64,
    ping_done: u64,
    /// The mixer answered a ping of this calibrator: the handles are live.
    bound: bool,
    /// Steps, and input frames, since the last answer of the mixer.
    silent_steps: u32,
    silent_in: f64,
    /// The driver stalled: wait for the answer to a ping sent after this one.
    hold: Option<u64>,
    lead_extra: u64,
    /// Commands waiting for a free slot of the mixer queue, in order.
    outbox: VecDeque<Out>,
    // Engine state.
    /// Epoch of the streams this calibration runs on (the newest one announced before
    /// the mixer answered).
    epoch: Option<u32>,
    /// A `Stopped` came before the mixer answered; steps it has waited for a `Reopened`.
    stop_seen: bool,
    stop_wait: u32,
    snap: Option<ClockSnapshot>,
    /// Stamp sources (input, output) of the last snapshot.
    src: Option<(StampSource, StampSource)>,
    /// The stamp source changed since the last look.
    fell: bool,
    det_offset: Option<i64>,
    pitch_in: f64,
    interrupted: bool,
    // Tap.
    tap_on: bool,
    tap_origin: Option<u64>,
    tap_used: u64,
    tap_gap: bool,
    quiet_from: f64,
    /// Input clock (latest `Pitch` frame) when the tap was switched on.
    tap_on_pitch: f64,
    tap_resets: u32,
    scratch: Vec<f32>,
    level: Level,
    // Measurements.
    round: Option<Round>,
    round_seq: u32,
    glitches: u32,
    loud_retry: bool,
    blocked_checked: bool,
    check_unsent: bool,
    check_tries: u32,
    /// How long the next `Ready` waits for the clock lock, seconds.
    lock_wait_s: f64,
    /// Steps 2 to 4 ended without a latency: only the strums can give an offset
    /// (addendum A3).
    loop_refused: bool,
    /// The short round was analysed (accepted or refused): it does not run again.
    short_tried: bool,
    l_rt: Option<LoopFix>,
    beat: Option<BeatRun>,
    /// What the strums gave, while [`Phase::Settle`] waits for the lock.
    settling: Option<BiasEst>,
    details: Details,
    outcome: Option<Outcome>,
    inbox: Vec<EngineEvent>,
}

impl Calibrator {
    /// A calibration for the streams of `cfg.key`. `profile` is the saved profile, if any:
    /// when its key matches, step 0 checks it and the rest only runs if the path moved.
    pub fn new(cfg: CalibConfig, profile: Option<Profile>) -> Self {
        let fs_in = f64::from(cfg.key.fs_in);
        let fs_out = f64::from(cfg.key.fs_out);
        let bank = SoundBank::new(fs_out);
        let saved = profile.filter(|p| p.key == cfg.key);
        let prompt = if saved.is_some() {
            Prompt::Checking
        } else {
            Prompt::Quiet
        };
        Self {
            fs_in,
            fs_out,
            saved,
            probe_peak: true_peak(bank.get(SoundId::Probe)),
            click_peak: true_peak(bank.get(SoundId::Click))
                .max(true_peak(bank.get(SoundId::Accent))),
            rng: Mulberry32::new(cfg.seed),
            phase: Phase::Boot,
            prompt,
            fresh: true,
            now: None,
            t0: 0,
            gap: 0,
            ping_tag: cfg
                .tag_base
                .wrapping_add(TAG_PING0 + INSTANCES.fetch_add(1, Ordering::Relaxed) % PING_TAGS),
            ping_at: 0,
            ping_floor: 0,
            ping_out: false,
            ping_idle: 0,
            ping_sent_in: 0.0,
            dead: false,
            ping_sent: 0,
            ping_done: 0,
            bound: false,
            silent_steps: 0,
            silent_in: 0.0,
            hold: None,
            lead_extra: 0,
            outbox: VecDeque::new(),
            epoch: None,
            stop_seen: false,
            stop_wait: 0,
            snap: None,
            src: None,
            fell: false,
            det_offset: None,
            pitch_in: f64::NEG_INFINITY,
            interrupted: false,
            tap_on: false,
            tap_origin: None,
            tap_used: 0,
            tap_gap: false,
            quiet_from: f64::NEG_INFINITY,
            tap_on_pitch: f64::NEG_INFINITY,
            tap_resets: 0,
            scratch: vec![0.0; SCRATCH],
            level: Level::default(),
            round: None,
            round_seq: 0,
            glitches: 0,
            loud_retry: false,
            blocked_checked: false,
            check_unsent: false,
            check_tries: 0,
            lock_wait_s: LOCK_WAIT_S,
            loop_refused: false,
            short_tried: false,
            l_rt: None,
            beat: None,
            settling: None,
            details: Details {
                noise_floor_dbfs: None,
                input_peak_dbfs: None,
                clipped: false,
                quick: None,
                rounds: Vec::new(),
                l_rt_ms: None,
                l_rt_mad_ms: None,
                no_loop: false,
                beats_played: 0,
                strums_heard: 0,
                strums_used: 0,
                tendency_ms: None,
                tendency_mad_ms: None,
                combined_ms: None,
                combined_mad_ms: None,
                unresolved_offset_ms: None,
                judgeable: false,
                basis: Basis::None,
                clock_quality: ClockQuality::Acquiring,
                drift_ppm: 0.0,
                stamps_in: StampSource::Device,
                stamps_out: StampSource::Device,
                warnings: Vec::new(),
                seconds: 0.0,
            },
            outcome: None,
            inbox: Vec::new(),
            cfg,
        }
    }

    /// Drains `io.events` and steps. The events are consumed; use [`Calibrator::step`] to
    /// look at them too.
    pub fn step_io(&mut self, io: &mut EngineIo) -> CalibUpdate {
        let mut inbox = std::mem::take(&mut self.inbox);
        inbox.clear();
        while let Ok(e) = io.events.pop() {
            inbox.push(e);
        }
        let update = self.step(&mut io.mix, &mut io.tap, &inbox);
        self.inbox = inbox;
        update
    }

    /// One step: takes every engine event since the last call (all of them, in order),
    /// reads the tap, schedules sounds and moves the state machine. Call it at least
    /// every 100 ms; every 5 to 20 ms is typical. The tap holds 2 s: a longer pause
    /// during a probe round costs a repeat of the round.
    pub fn step(
        &mut self,
        mix: &mut MixerHandle,
        tap: &mut TapHandle,
        events: &[EngineEvent],
    ) -> CalibUpdate {
        let before = self.prompt;
        if !self.is_done() {
            let pitch_before = self.pitch_in;
            let answered = self.ping_done;
            for e in events {
                self.absorb(e);
            }
            // Input time since the last step: 0 while the input clock is not known (or
            // was just forgotten with the streams it belonged to).
            let passed = if pitch_before.is_finite() && self.pitch_in.is_finite() {
                (self.pitch_in - pitch_before).max(0.0)
            } else {
                0.0
            };
            if passed >= STALL_S * self.fs_in && self.hold.is_none() {
                // Much input time since the last step: the driver stalled, and the frame
                // counter in hand was read before that. Nothing is scheduled on it.
                self.hold = Some(self.ping_sent);
            }
            self.watch(answered, passed);
            if self.interrupted {
                self.abort(tap);
            } else {
                if self.phase == Phase::Boot {
                    tap.set(false);
                }
                self.drain_tap(tap);
                if self.hold.is_some_and(|mark| self.ping_done > mark) {
                    self.hold = None;
                }
                for _ in 0..8 {
                    if self.hold.is_some() || !self.advance(tap) {
                        break;
                    }
                }
            }
        }
        // Also once done: a stop that found the queue full still has to go out.
        self.flush(mix);
        if !self.is_done() {
            self.ping(mix);
            if self.dead {
                // Streams that were reopened or dropped under the calibration.
                self.warn(Warning::NoMixer);
                self.abort(tap);
            }
        }
        let changed = std::mem::take(&mut self.fresh) || self.prompt != before;
        CalibUpdate {
            prompt: self.prompt,
            changed,
            done: self.is_done(),
        }
    }

    /// Stops the sounds of the calibration, switches the tap off and ends with
    /// [`Outcome::Interrupted`]. No effect once done.
    pub fn cancel(&mut self, mix: &mut MixerHandle, tap: &mut TapHandle) {
        if self.is_done() {
            return;
        }
        self.abort(tap);
        self.flush(mix);
    }

    /// `true` when nothing waits for room in the mixer queue. After the end, a stop may
    /// still wait there if the queue was full: `step` sends it, so call `step` until
    /// this is `true` (with the default queue it always is).
    pub fn flushed(&self) -> bool {
        self.outbox.is_empty()
    }

    /// The calibration ended.
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// What to show now.
    pub fn prompt(&self) -> Prompt {
        self.prompt
    }

    /// How it ended. `None` while running.
    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// The numbers so far (complete once done).
    pub fn details(&self) -> &Details {
        &self.details
    }

    /// The click schedule of step 5, once it is known.
    pub fn beat_plan(&self) -> Option<BeatPlan> {
        self.beat.as_ref().map(|b| b.plan)
    }

    /// The configuration in use.
    pub fn config(&self) -> &CalibConfig {
        &self.cfg
    }

    // -------------------------------------------------------------- small helpers

    fn tag(&self, k: u32) -> u32 {
        self.cfg.tag_base.wrapping_add(k)
    }

    fn out_frames(&self, seconds: f64) -> u64 {
        (seconds.max(0.0) * self.fs_out).round() as u64
    }

    /// Frames between "now" and the earliest start a command can still make.
    fn lead(&self) -> u64 {
        self.out_frames(LEAD_S).max(3 * self.gap) + self.lead_extra
    }

    fn warn(&mut self, w: Warning) {
        if !self.details.warnings.contains(&w) {
            self.details.warnings.push(w);
        }
    }

    fn locked(&self) -> bool {
        self.snap
            .is_some_and(|s| s.quality() == ClockQuality::Locked)
    }

    /// The probe round a measurement of the loop starts with: the short one where it is
    /// configured and was not analysed yet, otherwise the full one.
    fn opening_round(&self) -> RoundKind {
        if self.cfg.short_probes > 0 && !self.short_tried {
            RoundKind::Short
        } else {
            RoundKind::First
        }
    }

    /// The saved profile was made on the stamps the clock map uses now.
    fn saved_holds(&self) -> bool {
        self.saved
            .as_ref()
            .zip(self.snap)
            .is_some_and(|(p, s)| p.holds_on(&s))
    }

    /// Ends as interrupted: stops what may still sound and switches the tap off.
    fn abort(&mut self, tap: &mut TapHandle) {
        let at = self.now.unwrap_or(0);
        for k in [TAG_BEAT, TAG_CHECK] {
            let tag = self.tag(k);
            self.out(MixCmd::StopTag { tag, at }, None, OutKind::Stop);
        }
        if let Some(tag) = self.round.as_ref().map(|r| r.tag) {
            self.out(MixCmd::StopTag { tag, at }, None, OutKind::Stop);
        }
        tap.set(false);
        self.close(Outcome::Interrupted, Prompt::Interrupted);
    }

    /// Queues `cmd` for the mixer. Past output frame `due` it is given up.
    fn out(&mut self, cmd: MixCmd, due: Option<OutFrame>, what: OutKind) {
        self.outbox.push_back(Out { cmd, due, what });
    }

    /// How long before its start frame a command must have left.
    fn send_margin(&self) -> u64 {
        self.out_frames(SEND_MARGIN_S).max(2 * self.gap)
    }

    /// Sends what waits, in order, as far as the mixer queue has room.
    fn flush(&mut self, mix: &mut MixerHandle) {
        while let Some(o) = self.outbox.front().copied() {
            if o.due.zip(self.now).is_some_and(|(due, now)| now > due) {
                self.outbox.pop_front();
                self.unsent(o.what);
                continue;
            }
            if mix.slots() == 0 {
                break;
            }
            if !mix.send(o.cmd) {
                // Refused with room in the queue: nothing will ever take a command.
                self.dead = true;
                self.outbox.clear();
                break;
            }
            self.outbox.pop_front();
            if let OutKind::Probe(k) = o.what
                && let Some(q) = self.round.as_mut().and_then(|r| r.queued.get_mut(k))
            {
                *q = true;
            }
        }
    }

    /// A command found no room in the mixer queue in time.
    fn unsent(&mut self, what: OutKind) {
        self.warn(Warning::QueueFull);
        match what {
            OutKind::Check => self.check_unsent = true,
            OutKind::Metronome => {
                if let Some(b) = self.beat.as_mut() {
                    b.refused = true;
                }
            }
            OutKind::Probe(_) | OutKind::Stop => {}
        }
    }

    /// Ends the calibration when the mixer does not answer, or when streams that stopped
    /// before it ever answered are not announced as reopened.
    fn watch(&mut self, answered: u64, passed: f64) {
        if self.stop_seen {
            self.stop_wait += 1;
            if self.stop_wait > STOP_WAIT_STEPS {
                self.interrupted = true;
            }
        }
        if self.ping_done != answered {
            self.silent_steps = 0;
            self.silent_in = 0.0;
            return;
        }
        self.silent_steps += 1;
        // A stalled driver must not count as a silent mixer: one step adds little.
        self.silent_in += passed.min(STALL_S * self.fs_in);
        if self.silent_in >= MIXER_DEAD_S * self.fs_in
            || self.silent_steps >= self.cfg.max_silent_steps.max(1)
        {
            self.warn(Warning::NoMixer);
            self.interrupted = true;
        }
    }

    /// An event of clock epoch `epoch`: `true` if it belongs to this calibration.
    ///
    /// The event queue outlives a reopen, so a calibrator built on fresh handles first
    /// reads what the streams before it left there, then the `Reopened` notice. Until
    /// the mixer answers a ping, the newest epoch seen is the one this calibration runs
    /// on and older events are skipped. After that, a newer epoch means that the streams
    /// were reopened under the calibration: it is interrupted.
    fn gate(&mut self, epoch: u32) -> bool {
        match self.epoch {
            Some(mine) if epoch == mine => true,
            Some(mine) if epoch < mine => false,
            Some(_) if self.bound => {
                self.interrupted = true;
                false
            }
            earlier => {
                if earlier.is_some() {
                    // What was learned so far belongs to streams that are gone.
                    self.snap = None;
                    self.src = None;
                    self.fell = false;
                    self.det_offset = None;
                    self.pitch_in = f64::NEG_INFINITY;
                    self.quiet_from = f64::NEG_INFINITY;
                    self.tap_gap = false;
                }
                self.stop_seen = false;
                self.epoch = Some(epoch);
                true
            }
        }
    }

    // -------------------------------------------------------------- events

    fn absorb(&mut self, e: &EngineEvent) {
        match *e {
            EngineEvent::Mix(m) => {
                // Nothing the mixer said before the first ping of this calibrator is
                // an answer to it.
                if self.ping_sent > 0 {
                    self.on_mix(m);
                }
            }
            EngineEvent::Clock(s) => {
                if self.gate(s.epoch()) {
                    self.on_clock(s);
                }
            }
            EngineEvent::Pitch(p) => {
                if !self.gate(p.epoch) {
                    return;
                }
                self.note_offset(p.t_in - p.frame.t * self.fs_in);
                if p.t_in.is_finite() && p.t_in > self.pitch_in {
                    self.pitch_in = p.t_in;
                    if !self.quiet_from.is_finite() {
                        self.quiet_from = p.t_in;
                    }
                }
            }
            EngineEvent::Onset(n) => {
                if self.gate(n.epoch) {
                    self.on_onset(&n);
                }
            }
            EngineEvent::Step(st) => {
                if self.gate(st.epoch) {
                    self.on_step(&st);
                }
            }
            EngineEvent::Unjudged {
                epoch,
                from_in,
                to_in,
            } => {
                if self.gate(epoch) {
                    self.on_unjudged(from_in, to_in);
                }
            }
            EngineEvent::Reopened { epoch } => {
                self.gate(epoch);
            }
            EngineEvent::Stopped(_) => {
                if self.bound {
                    self.interrupted = true;
                } else {
                    // Possibly old news: see `watch`.
                    self.stop_seen = true;
                    self.stop_wait = 0;
                }
            }
            EngineEvent::Dropped {
                ring: Ring::Tap, ..
            } => self.tap_gap = true,
            _ => {}
        }
    }

    /// A clock step. Any step voids a probe round. A step in step 5 is expected to bring
    /// its unjudged window. A fallback to other stamps moves `in_of_out` by a constant:
    /// see `on_fallback`.
    fn on_step(&mut self, st: &ClockStep) {
        if let Some(r) = self.round.as_mut() {
            r.glitch = true;
        }
        if let Some(b) = self.beat.as_mut() {
            b.step_open += 1;
        }
        if st.cause == StepCause::Fallback {
            self.fell = true;
        }
    }

    /// `x` = input frame minus detector time of one event: a whole number, constant
    /// within an epoch.
    fn note_offset(&mut self, x: f64) {
        if !x.is_finite() {
            return;
        }
        let offset = x.round() as i64;
        if self.det_offset.is_some_and(|old| old != offset)
            && let Some(r) = self.round.as_mut()
        {
            r.glitch = true;
        }
        self.det_offset = Some(offset);
    }

    fn on_clock(&mut self, s: ClockSnapshot) {
        let src = (s.source(Side::In), s.source(Side::Out));
        if self.src.is_some_and(|old| old != src) {
            self.fell = true;
        }
        self.src = Some(src);
        self.snap = Some(s);
        if let (Some(r), Some(now)) = (self.round.as_mut(), self.now) {
            // A round starts on a locked map, and its latency is read off that map. One
            // that lost its lock under the probes is no longer the map the round
            // started on, whether or not a step was announced.
            if s.quality() != ClockQuality::Locked {
                r.glitch = true;
            }
            let f = now as f64;
            r.pts.push((f, s.in_of_out(f)));
        }
    }

    fn on_onset(&mut self, n: &OnsetNote) {
        let (det, strum) = match n.event {
            OnsetEvent::Strum(s) => (s.t, true),
            OnsetEvent::Heard { t, .. } => (t, false),
        };
        self.note_offset(n.t_in - det);
        if self.phase != Phase::Beat || !strum {
            return;
        }
        self.details.strums_heard += 1;
        if let Some(b) = self.beat.as_mut()
            && n.judged
            && n.t_in.is_finite()
        {
            b.onsets.push(n.t_in);
            b.dirty = true;
        }
    }

    /// The engine withdrew the judgement of a window around a clock step.
    fn on_unjudged(&mut self, from_in: f64, to_in: f64) {
        if let Some(b) = self.beat.as_mut() {
            let pad = 0.25 * self.fs_in;
            b.step_open = b.step_open.saturating_sub(1);
            b.onsets.retain(|t| *t < from_in || *t > to_in);
            b.beats_in
                .retain(|g| *g < from_in - pad || *g > to_in + pad);
            b.dirty = true;
        }
    }

    fn on_mix(&mut self, m: MixEvent) {
        let (ping, beat) = (self.ping_tag, self.tag(TAG_BEAT));
        match m {
            MixEvent::Late { tag, by } if tag == ping => {
                self.ping_out = false;
                self.bound = true;
                self.ping_done = self.ping_sent;
                let first = self.ping_at + u64::from(by);
                if by == u32::MAX {
                    // The counter is at least this far: ask again from there.
                    self.ping_floor = first;
                } else {
                    self.set_now(first);
                }
            }
            MixEvent::Started { tag, at } if tag == ping => {
                // Only possible when the command reached the very block that starts at `at`.
                self.ping_out = false;
                self.bound = true;
                self.ping_done = self.ping_sent;
                self.set_now(at);
            }
            MixEvent::Beat { tag, index, at } if tag == beat => self.on_beat(index, at),
            MixEvent::Rejected { tag } if tag == beat => {
                if let Some(b) = self.beat.as_mut() {
                    b.refused = true;
                }
            }
            MixEvent::Late { tag, by } => {
                let ours = tag == beat || self.round.as_ref().is_some_and(|r| r.tag == tag);
                if ours {
                    self.lead_extra = self.lead_extra.max(u64::from(by) + self.gap);
                    self.warn(Warning::LateCommand);
                    if let Some(r) = self.round.as_mut().filter(|r| r.tag == tag) {
                        r.late += 1;
                    }
                }
            }
            MixEvent::Started { tag, at } => {
                if let Some(r) = self.round.as_mut().filter(|r| r.tag == tag)
                    && let Some(k) = r.ats.iter().position(|a| *a == at)
                {
                    r.started[k] = true;
                }
            }
            _ => {}
        }
    }

    fn set_now(&mut self, first: OutFrame) {
        match self.now {
            None => {
                self.now = Some(first);
                self.t0 = first;
            }
            Some(prev) if first > prev => {
                if self.hold.is_none() {
                    self.gap = (first - prev).max(self.gap - self.gap / 8);
                }
                self.now = Some(first);
            }
            Some(_) => {}
        }
    }

    fn on_beat(&mut self, index: u32, at: OutFrame) {
        let snap = self.snap;
        let Some(b) = self.beat.as_mut() else {
            return;
        };
        let of = b.plan.beats.min(255) as u8;
        if index < b.plan.count_in {
            self.prompt = Prompt::StrumMuted {
                count_in: (b.plan.count_in - index).min(255) as u8,
                beat: 0,
                of,
            };
            return;
        }
        self.prompt = Prompt::StrumMuted {
            count_in: 0,
            beat: (index - b.plan.count_in + 1).min(255) as u8,
            of,
        };
        self.details.beats_played += 1;
        // Only a locked map places a beat well enough to pair a strum with it.
        if let Some(s) = snap.filter(|s| s.quality() == ClockQuality::Locked) {
            b.beats_in.push(s.in_of_out(at as f64));
            b.dirty = true;
        }
    }

    // -------------------------------------------------------------- output clock, tap

    /// Asks the mixer for its frame counter (see the module documentation).
    fn ping(&mut self, mix: &mut MixerHandle) {
        // The calibration's own commands go first: one free slot is enough for a ping,
        // and a queue of one slot must not be held by it.
        if !self.outbox.is_empty() {
            return;
        }
        if self.dead {
            return;
        }
        let again = self.ping_out;
        if again {
            // Same `at` as the one that got no answer: either answer reads the same.
            self.ping_idle += 1;
            let waited_in = self.silent_in - self.ping_sent_in;
            if self.ping_idle < PING_RESEND_STEPS && waited_in < PING_RESEND_S * self.fs_in {
                return;
            }
        } else {
            self.ping_at = self
                .now
                .map_or(0, |n| n.saturating_sub(PING_BACK))
                .max(self.ping_floor);
        }
        if mix.slots() == 0 {
            return;
        }
        let cmd = MixCmd::Play {
            tag: self.ping_tag,
            sound: SoundId::Click,
            at: self.ping_at,
            gain: 0.0,
        };
        if mix.send(cmd) {
            self.ping_out = true;
            self.ping_idle = 0;
            self.ping_sent_in = self.silent_in;
            if !again {
                self.ping_sent += 1;
            }
        } else {
            // Refused with room in the queue: see `flush`.
            self.dead = true;
        }
    }

    fn drain_tap(&mut self, tap: &mut TapHandle) {
        loop {
            let n = tap.read(&mut self.scratch);
            if n == 0 {
                break;
            }
            if !self.tap_on {
                // Left over, or the stage has not seen "off" yet: wait on from here.
                self.quiet_from = self.pitch_in;
                continue;
            }
            if self.tap_origin.is_none() {
                self.tap_origin = Some(tap.origin());
                self.tap_used = 0;
            }
            let block = &self.scratch[..n];
            match self.phase {
                Phase::Probing => {
                    if let Some(r) = self.round.as_mut() {
                        r.rec.extend_from_slice(block);
                    }
                }
                Phase::Silence { .. } | Phase::BlockedCheck { .. } => self.level.add(block),
                _ => {}
            }
            self.tap_used += n as u64;
        }
    }

    fn enter_tap_off(&mut self, tap: &mut TapHandle, now: OutFrame, next: Next) {
        tap.set(false);
        self.tap_on = false;
        self.tap_origin = None;
        self.quiet_from = self.pitch_in;
        self.phase = Phase::TapOff { since: now, next };
    }

    fn enter_silence(&mut self, now: OutFrame) {
        self.level = Level::default();
        self.prompt = Prompt::Quiet;
        let until = (self.t0 + self.out_frames(self.cfg.silence_s))
            .max(now + self.out_frames(SILENCE_MIN_S));
        self.phase = Phase::Silence { until };
    }

    // -------------------------------------------------------------- the machine

    /// Moves at most one phase forward. `true`: something changed, look again.
    fn advance(&mut self, tap: &mut TapHandle) -> bool {
        let Some(now) = self.now else {
            return false;
        };
        if std::mem::take(&mut self.fell) && self.on_fallback(now, tap) {
            return true;
        }
        match self.phase {
            Phase::Boot => {
                let next = if self.saved.is_some() {
                    Next::Round(RoundKind::Quick)
                } else {
                    Next::Silence
                };
                self.enter_tap_off(tap, now, next);
                true
            }
            Phase::TapOff { since, next } => {
                let waited = now - since >= self.out_frames(TAP_OFF_S);
                let quiet = self.pitch_in - self.quiet_from >= TAP_QUIET_S * self.fs_in;
                let timeout = now - since >= self.out_frames(TAP_OFF_MAX_S);
                if !(waited && quiet || timeout) {
                    return false;
                }
                if !quiet {
                    self.warn(Warning::TapNotIdle);
                }
                tap.set(true);
                self.tap_on = true;
                self.tap_origin = None;
                self.tap_used = 0;
                self.tap_gap = false;
                self.tap_on_pitch = self.pitch_in;
                match next {
                    Next::Silence => self.enter_silence(now),
                    Next::Round(RoundKind::Quick) => {
                        self.phase = Phase::Ready {
                            from: now,
                            until: (self.t0 + self.out_frames(QUICK_WAIT_S))
                                .max(now + self.out_frames(SILENCE_MIN_S)),
                            kind: RoundKind::Quick,
                        };
                    }
                    Next::Round(kind) => {
                        let wait = std::mem::replace(&mut self.lock_wait_s, LOCK_WAIT_S);
                        self.phase = Phase::Ready {
                            from: now,
                            until: now + self.out_frames(wait),
                            kind,
                        };
                    }
                }
                true
            }
            Phase::Silence { until } => {
                if now < until {
                    return false;
                }
                self.end_silence(now, tap);
                true
            }
            Phase::BlockedCheck { until } => {
                if self.check_unsent && self.check_tries < CHECK_TRIES {
                    // The check probe never left: plan it again.
                    self.check_tries += 1;
                    self.check_unsent = false;
                    self.blocked_checked = false;
                    self.end_silence(now, tap);
                    return true;
                }
                if now < until {
                    return false;
                }
                // Without the probe there is no verdict: "blocked" is only said when it
                // played and the input stayed at zero.
                if self.level.nonzero == 0 && !self.check_unsent {
                    tap.set(false);
                    self.close(Outcome::MicBlocked, Prompt::MicBlocked);
                } else {
                    // A path that is digitally silent until something plays (a loopback
                    // cable, a noise gate): not blocked. There is no floor to report.
                    self.phase = Phase::Ready {
                        from: now,
                        until: now + self.out_frames(LOCK_WAIT_S),
                        kind: self.opening_round(),
                    };
                }
                true
            }
            Phase::Ready { from, until, kind } => {
                // The first tap sample must have been captured after the tap was switched
                // on. An older origin is the one of an earlier recording (the capture
                // stage never saw the tap off): samples would be counted from the wrong
                // frame, so the tap is reset instead.
                let origin = self.tap_origin.zip(self.det_offset);
                let stale = self.tap_on_pitch.is_finite()
                    && origin.is_some_and(|(o, off)| {
                        ((o as i64 + off) as f64)
                            < self.tap_on_pitch - TAP_ORIGIN_SLACK_S * self.fs_in
                    });
                if (stale || self.tap_gap) && self.tap_resets < TAP_RESETS {
                    if stale {
                        self.warn(Warning::TapNotIdle);
                    }
                    self.tap_resets += 1;
                    self.enter_tap_off(tap, now, Next::Round(kind));
                    return true;
                }
                // Past the resets, a tap that is still wrong cannot be used at all.
                let tap_bad = stale || self.tap_gap;
                let tap_ok = origin.is_some() && self.tap_on_pitch.is_finite() && !tap_bad;
                if self.locked() && tap_ok && now >= from {
                    if kind == RoundKind::Quick && !self.saved_holds() {
                        // The saved latency was measured on other stamps: it is off by
                        // a constant on this map, whatever three probes would say.
                        self.warn(Warning::ProfileOtherStamps);
                        self.saved_not_kept(None);
                        self.enter_silence(now);
                        return true;
                    }
                    self.start_round(kind, now);
                    return true;
                }
                if now < until && !(tap_bad && self.locked()) {
                    return false;
                }
                match kind {
                    RoundKind::Quick => {
                        self.details.quick = Some(QuickCheck {
                            profile_l_rt_ms: self.saved.as_ref().and_then(|p| p.l_rt_ms),
                            measured_ms: None,
                            delta_ms: None,
                            kept: false,
                        });
                        self.enter_silence(now);
                    }
                    _ if !self.locked() => {
                        tap.set(false);
                        self.close(Outcome::Unstable, Prompt::RhythmLater);
                    }
                    _ => {
                        self.warn(Warning::NoInputClock);
                        self.give_up_loop(now, tap);
                    }
                }
                true
            }
            Phase::Probing => self.advance_round(now, tap),
            Phase::Beat => self.advance_beat(now, tap),
            Phase::BeatWait { from, until } => {
                if self.locked() && now >= from {
                    self.start_beat(now, tap);
                    return true;
                }
                if now < until {
                    return false;
                }
                tap.set(false);
                self.close(Outcome::Unstable, Prompt::RhythmLater);
                true
            }
            Phase::Settle { until } => {
                let quality = self.snap.map(|s| s.quality());
                if quality == Some(ClockQuality::Acquiring) && now < until {
                    return false;
                }
                let est = self.settling.take();
                self.close_calibrated(est, tap);
                true
            }
            Phase::Done => false,
        }
    }

    /// The clock map changed its stamp source: `in_of_out` moved by a constant (the time
    /// between a callback and its sound). A probe round in progress is void. In step 5,
    /// the pairs so far and the latency measured before are on the old map, the pairs to
    /// come on the new one: both are measured again. `true`: the phase changed.
    fn on_fallback(&mut self, now: OutFrame, tap: &mut TapHandle) -> bool {
        match self.phase {
            Phase::Probing => {
                self.warn(Warning::StampFallback);
                if let Some(r) = self.round.as_mut() {
                    r.glitch = true;
                }
                false
            }
            // The latency, and the strums with it, were read on the old map.
            Phase::Beat | Phase::Settle { .. } => {
                self.warn(Warning::StampFallback);
                self.void_beat(now, tap);
                true
            }
            // A round waits for the lock: the map now acquires again, on the new stamps,
            // and the wait that was planned may end before it has. (A quick check does
            // not wait: its profile no longer holds, and the silence step comes next.)
            Phase::Ready { from, until, kind } if kind != RoundKind::Quick => {
                self.phase = Phase::Ready {
                    from,
                    until: until.max(now + self.out_frames(RELOCK_WAIT_S)),
                    kind,
                };
                false
            }
            _ => false,
        }
    }

    /// Step 5 cannot be used, and neither can a latency from before it. Within the
    /// budget of one repeat (shared with the probe rounds) the measurement starts again
    /// on the map now in force; past it the calibration ends without a verdict.
    fn void_beat(&mut self, now: OutFrame, tap: &mut TapHandle) {
        let tag = self.tag(TAG_BEAT);
        self.outbox.retain(|o| o.what != OutKind::Metronome);
        self.out(MixCmd::StopTag { tag, at: now }, None, OutKind::Stop);
        self.beat = None;
        self.settling = None;
        self.details.beats_played = 0;
        self.details.strums_heard = 0;
        self.l_rt = None;
        self.details.l_rt_ms = None;
        self.details.l_rt_mad_ms = None;
        if let Some(q) = self.details.quick.as_mut() {
            // A kept profile whose rhythm was being measured again: it no longer holds.
            q.kept = false;
        }
        self.glitches += 1;
        if self.glitches > 1 {
            tap.set(false);
            self.close(Outcome::Unstable, Prompt::RhythmLater);
            return;
        }
        self.prompt = Prompt::Quiet;
        if self.loop_refused {
            self.phase = Phase::BeatWait {
                from: now + self.out_frames(GLITCH_PAUSE_S),
                until: now + self.out_frames(RELOCK_WAIT_S),
            };
        } else {
            self.lock_wait_s = RELOCK_WAIT_S;
            self.enter_tap_off(tap, now, Next::Round(RoundKind::First));
        }
    }

    fn end_silence(&mut self, now: OutFrame, tap: &mut TapHandle) {
        let level = self.level;
        if level.n < (0.25 * self.fs_in) as u64 || level.nonzero == 0 {
            if self.blocked_checked {
                tap.set(false);
                self.close(Outcome::MicBlocked, Prompt::MicBlocked);
                return;
            }
            self.blocked_checked = true;
            self.check_unsent = false;
            let at = now + self.lead();
            let cmd = MixCmd::Play {
                tag: self.tag(TAG_CHECK),
                sound: SoundId::Probe,
                at,
                gain: level_gain(self.cfg.probe_dbfs, self.probe_peak),
            };
            let due = at.saturating_sub(self.send_margin());
            self.out(cmd, Some(due), OutKind::Check);
            self.phase = Phase::BlockedCheck {
                until: at + self.out_frames(BLOCK_CHECK_S),
            };
            return;
        }
        if level.clip_fraction() > CLIP_FRACTION {
            self.details.clipped = true;
            if !self.loud_retry {
                self.loud_retry = true;
                self.prompt = Prompt::TooLoud;
                self.level = Level::default();
                self.phase = Phase::Silence {
                    until: now + self.out_frames(LOUD_RETRY_S),
                };
                return;
            }
            self.warn(Warning::InputClipped);
        }
        self.details.noise_floor_dbfs = level.floor_dbfs();
        self.details.input_peak_dbfs = level.peak_dbfs();
        self.phase = Phase::Ready {
            from: now,
            until: now + self.out_frames(LOCK_WAIT_S),
            kind: self.opening_round(),
        };
    }

    // -------------------------------------------------------------- probe rounds

    fn start_round(&mut self, kind: RoundKind, now: OutFrame) {
        let (count, dbfs) = match kind {
            RoundKind::Quick => (self.cfg.quick_probes, self.cfg.probe_dbfs),
            RoundKind::Short => (self.cfg.short_probes, self.cfg.probe_dbfs),
            RoundKind::First => (self.cfg.probes, self.cfg.probe_dbfs),
            RoundKind::Retry => (self.cfg.probes, self.cfg.retry_dbfs),
        };
        let dbfs = dbfs.min(MAX_DBFS);
        let gain = level_gain(dbfs, self.probe_peak);
        let tag = self.tag(TAG_ROUND0 + self.round_seq);
        self.round_seq += 1;

        let gaps = match kind {
            RoundKind::Short => spread_gaps(
                count.saturating_sub(1) as usize,
                self.cfg.short_gap_s,
                self.cfg.short_gap_jitter_s,
                &mut self.rng,
            ),
            _ => (1..count)
                .map(|_| {
                    let jitter = (self.rng.next_f64() * 2.0 - 1.0) * self.cfg.probe_gap_jitter_s;
                    self.cfg.probe_gap_s + jitter
                })
                .collect(),
        };
        let mut ats = Vec::with_capacity(count as usize);
        let mut at = now + self.lead();
        for k in 0..count as usize {
            if k > 0 {
                at += self.out_frames(gaps[k - 1].max(4.0 * PROBE_S));
            }
            ats.push(at);
        }
        // The whole round is planned now and leaves as the mixer queue has room: at once
        // with the usual queue, one command per callback with a queue of one slot.
        let margin = self.send_margin();
        for (k, &at) in ats.iter().enumerate() {
            let cmd = MixCmd::Play {
                tag,
                sound: SoundId::Probe,
                at,
                gain,
            };
            self.out(cmd, Some(at.saturating_sub(margin)), OutKind::Probe(k));
        }
        let full = self.cfg.loopback.search_max_s;
        let search_s = match (kind, self.saved.as_ref().and_then(|p| p.l_rt_ms)) {
            // The saved latency is known: a short window is enough to confirm it.
            (RoundKind::Quick, Some(ms)) => (ms / 1.0e3 + 0.06).clamp(0.08, full.max(0.08)),
            _ => full,
        };
        let last = ats.last().copied().unwrap_or(now);
        let seconds = (last - now) as f64 / self.fs_out + search_s + 1.0;
        self.round = Some(Round {
            kind,
            tag,
            level_dbfs: dbfs,
            queued: vec![false; ats.len()],
            started: vec![false; ats.len()],
            live: vec![None; ats.len()],
            ats,
            late: 0,
            rec: Vec::with_capacity((seconds * self.fs_in) as usize),
            rec_det0: self.tap_origin.unwrap_or(0) + self.tap_used,
            pts: Vec::new(),
            glitch: false,
            search_s,
            deadline: last + self.out_frames(search_s + PROBE_S + ROUND_GRACE_S),
        });
        let count = count.min(255) as u8;
        self.prompt = match kind {
            RoundKind::Quick => Prompt::Checking,
            RoundKind::Short | RoundKind::First => Prompt::WillBeep { count },
            RoundKind::Retry => Prompt::WillBeepLouder { count },
        };
        self.phase = Phase::Probing;
    }

    fn advance_round(&mut self, now: OutFrame, tap: &mut TapHandle) -> bool {
        let snap = self.snap;
        let offset = self.det_offset;
        let tap_gap = self.tap_gap;
        let fs_in = self.fs_in;
        let Some(r) = self.round.as_mut() else {
            self.phase = Phase::Done;
            return false;
        };
        if let Some(s) = snap {
            for (k, at) in r.ats.iter().enumerate() {
                if r.live[k].is_none() && *at <= now {
                    r.live[k] = Some(s.in_of_out(*at as f64));
                }
            }
        }
        if r.glitch || tap_gap {
            return self.round_glitch(now, tap);
        }
        let covered = match (r.ats.last(), snap, offset) {
            (Some(&last), Some(s), Some(off)) => {
                let need = s.in_of_out(last as f64) + (r.search_s + PROBE_S + 0.005) * fs_in;
                let have = (r.rec_det0 as i64 + off) as f64 + r.rec.len() as f64;
                have >= need
            }
            _ => false,
        };
        if !covered && now < r.deadline {
            return false;
        }
        self.finish_round(now, tap);
        true
    }

    /// A clock step or a tap overflow: the round is void. Repeat it once.
    fn round_glitch(&mut self, now: OutFrame, tap: &mut TapHandle) -> bool {
        let Some(r) = self.round.take() else {
            return false;
        };
        self.outbox.retain(|o| !matches!(o.what, OutKind::Probe(_)));
        self.out(
            MixCmd::StopTag {
                tag: r.tag,
                at: now,
            },
            None,
            OutKind::Stop,
        );
        self.warn(Warning::RoundGlitch);
        self.details.rounds.push(RoundDetails {
            kind: r.kind,
            level_dbfs: r.level_dbfs,
            sent: r.queued.iter().filter(|q| **q).count() as u32,
            started: r.started.iter().filter(|s| **s).count() as u32,
            late: r.late,
            found: None,
            l_rt_ms: None,
            mad_ms: None,
            per_probe_ms: Vec::new(),
            refused: None,
            fit_vs_live_ms: None,
            fitted: false,
            glitch: true,
        });
        self.glitches += 1;
        if self.glitches <= 1 {
            // A map that is not locked now (it lost its lock, or fell back to other
            // stamps) has a whole lock window to go through before the repeat.
            let wait = if self.locked() {
                LOCK_WAIT_S
            } else {
                RELOCK_WAIT_S
            };
            if self.tap_gap {
                self.tap_resets = 0;
                self.lock_wait_s = wait;
                self.enter_tap_off(tap, now, Next::Round(r.kind));
            } else {
                self.phase = Phase::Ready {
                    from: now + self.out_frames(GLITCH_PAUSE_S),
                    until: now + self.out_frames(GLITCH_PAUSE_S + wait),
                    kind: r.kind,
                };
            }
        } else if r.kind == RoundKind::Quick {
            self.saved_not_kept(None);
            if self.tap_gap {
                self.enter_tap_off(tap, now, Next::Silence);
            } else {
                self.enter_silence(now);
            }
        } else {
            self.give_up_loop(now, tap);
        }
        true
    }

    fn finish_round(&mut self, now: OutFrame, tap: &mut TapHandle) {
        let Some(r) = self.round.take() else {
            return;
        };
        self.outbox.retain(|o| !matches!(o.what, OutKind::Probe(_)));
        let (result, detail) = self.analyse(&r);
        if detail
            .fit_vs_live_ms
            .is_some_and(|d| d > self.cfg.fit_warn_ms)
        {
            self.warn(Warning::ClockFitMismatch);
        }
        self.details.rounds.push(detail);
        match r.kind {
            RoundKind::Quick => self.finish_quick(&r, &result, now, tap),
            RoundKind::Short => {
                self.short_tried = true;
                match result {
                    Ok(fix) => {
                        self.l_rt = Some(fix);
                        self.after_loop(now, tap);
                    }
                    // Whatever the short round could not do, the full round decides.
                    Err(_) => self.start_round(RoundKind::First, now),
                }
            }
            RoundKind::First | RoundKind::Retry => match result {
                Ok(fix) => {
                    self.l_rt = Some(fix);
                    self.after_loop(now, tap);
                }
                Err(CalError::TooFewProbes { .. }) if r.kind == RoundKind::First => {
                    self.start_round(RoundKind::Retry, now);
                }
                Err(_) => self.give_up_loop(now, tap),
            },
        }
    }

    /// Step 3: the latency of one round, off the real-time path.
    fn analyse(&self, r: &Round) -> (Result<LoopFix, CalError>, RoundDetails) {
        let to_ms = 1.0e3 / self.fs_in;
        let started: Vec<usize> = (0..r.ats.len()).filter(|k| r.started[*k]).collect();
        let mut detail = RoundDetails {
            kind: r.kind,
            level_dbfs: r.level_dbfs,
            sent: r.queued.iter().filter(|q| **q).count() as u32,
            started: started.len() as u32,
            late: r.late,
            found: None,
            l_rt_ms: None,
            mad_ms: None,
            per_probe_ms: vec![None; started.len()],
            refused: None,
            fit_vs_live_ms: None,
            fitted: false,
            glitch: false,
        };
        let min_found = match r.kind {
            RoundKind::Quick => 2.min(self.cfg.quick_probes.max(1) as usize),
            RoundKind::Short => self.cfg.short_min_found.max(1) as usize,
            RoundKind::First | RoundKind::Retry => self.cfg.loopback.min_found,
        };
        let (Some(snap), Some(offset)) = (self.snap, self.det_offset) else {
            let e = CalError::TooFewProbes {
                found: 0,
                needed: min_found,
            };
            detail.refused = Some(e.clone());
            return (Err(e), detail);
        };

        // in_of_out over the window: the fitted line, or the last snapshot.
        let span = match (r.pts.first(), r.pts.last()) {
            (Some(a), Some(b)) => b.0 - a.0,
            _ => 0.0,
        };
        let nominal = self.fs_in / self.fs_out;
        let fit = (r.pts.len() >= FIT_MIN_POINTS && span >= FIT_MIN_SPAN_S * self.fs_out)
            .then(|| robust_line(&r.pts))
            .flatten()
            .filter(|l| (l.slope / nominal - 1.0).abs() < 2.0e-3);
        detail.fitted = fit.is_some();
        let f_ref = r.ats.first().map_or(0.0, |a| *a as f64);
        let line = fit.unwrap_or_else(|| Line::of_snapshot(&snap, f_ref));
        detail.fit_vs_live_ms = started
            .iter()
            .filter_map(|&k| r.live[k].map(|g| (line.at(r.ats[k] as f64) - g).abs() * to_ms))
            .reduce(f64::max);

        let g0 = (r.rec_det0 as i64 + offset) as f64;
        let pre = self.cfg.pre_roll_s.max(0.0) * self.fs_in;
        let emits: Vec<f64> = started
            .iter()
            .map(|&k| line.at(r.ats[k] as f64) - g0 - pre)
            .collect();
        let cfg = LoopbackConfig {
            search_max_s: r.search_s + self.cfg.pre_roll_s.max(0.0),
            min_found,
            ..self.cfg.loopback.clone()
        };
        let probe = Probe::new(self.fs_in);
        match measure_loopback(&r.rec, &emits, &probe, &cfg) {
            Ok(lb) => {
                detail.found = Some(lb.found as u32);
                detail.per_probe_ms = lb
                    .per_probe
                    .iter()
                    .map(|p| p.map(|x| (x - pre) * to_ms))
                    .collect();
                let mut fix = LoopFix {
                    frames: lb.latency - pre,
                    mad: lb.mad,
                };
                if r.kind == RoundKind::Short {
                    // The probes of this round are close enough for a window to hold a
                    // neighbour's arrival: only the probes that `vouched` can tell from
                    // their neighbours count. A probe that is not known to have played
                    // when planned has no window, and could be in any: none count then.
                    let complete = started.len() == r.ats.len() && r.late == 0;
                    let own = if complete {
                        let tol = self.cfg.loopback.max_mad_ms * 1.0e-3 * self.fs_in;
                        vouched(&lb.per_probe, &emits, tol)
                    } else {
                        Vec::new()
                    };
                    if own.len() < min_found {
                        let e = CalError::TooFewProbes {
                            found: own.len(),
                            needed: min_found,
                        };
                        detail.refused = Some(e.clone());
                        return (Err(e), detail);
                    }
                    let (lag, mad) = median_mad(&own);
                    fix = LoopFix {
                        frames: lag - pre,
                        mad,
                    };
                }
                detail.l_rt_ms = Some(fix.frames * to_ms);
                detail.mad_ms = Some(fix.mad * to_ms);
                (Ok(fix), detail)
            }
            Err(e) => {
                if let CalError::TooFewProbes { found, .. } = &e {
                    detail.found = Some(*found as u32);
                }
                detail.refused = Some(e.clone());
                (Err(e), detail)
            }
        }
    }

    fn saved_not_kept(&mut self, measured_ms: Option<f64>) {
        let saved = self.saved.as_ref().and_then(|p| p.l_rt_ms);
        self.details.quick = Some(QuickCheck {
            profile_l_rt_ms: saved,
            measured_ms,
            delta_ms: measured_ms.zip(saved).map(|(m, s)| m - s),
            kept: false,
        });
    }

    /// Step 0: keep the saved profile, or go on with the whole calibration.
    fn finish_quick(
        &mut self,
        r: &Round,
        result: &Result<LoopFix, CalError>,
        now: OutFrame,
        tap: &mut TapHandle,
    ) {
        let Some(saved) = self.saved.clone() else {
            self.enter_silence(now);
            return;
        };
        let measured_ms = result.as_ref().ok().map(|f| f.frames * 1.0e3 / self.fs_in);
        let kept = match (saved.l_rt_ms, result) {
            // A latency that may not be judged with was not valid when it was saved (the
            // clock map was degraded): three probes do not make it so. Measured again.
            (Some(l), Ok(_)) => {
                saved.judgeable
                    && measured_ms.is_some_and(|m| (m - l).abs() <= self.cfg.quick_tolerance_ms)
            }
            (Some(_), Err(_)) => false,
            // Saved without a loop: still none (and the input is alive) keeps it. What
            // the strums gave cannot be checked without the player.
            (None, Err(CalError::TooFewProbes { .. })) => r.rec.iter().any(|x| *x != 0.0),
            (None, _) => false,
        };
        // And only on the stamps it was made on (they may have changed in the round).
        let same_stamps = self.saved_holds();
        if kept && !same_stamps {
            self.warn(Warning::ProfileOtherStamps);
        }
        let kept = kept && same_stamps;
        if !kept {
            self.saved_not_kept(measured_ms);
            self.enter_silence(now);
            return;
        }
        self.details.quick = Some(QuickCheck {
            profile_l_rt_ms: saved.l_rt_ms,
            measured_ms,
            delta_ms: measured_ms.zip(saved.l_rt_ms).map(|(m, s)| m - s),
            kept: true,
        });
        // "Try again later": a kept profile without a loop that cannot be judged with
        // yet gets the strums again. (With a loop there is nothing the strums could add:
        // a kept latency is judged with as it is.)
        if self.cfg.retry_rhythm
            && self.cfg.beat_step != BeatStep::Never
            && saved.l_rt_ms.is_none()
            && !saved.judgeable
        {
            self.l_rt = None;
            self.loop_refused = true;
            self.start_beat(now, tap);
            return;
        }
        tap.set(false);
        self.details.l_rt_ms = saved.l_rt_ms;
        self.details.l_rt_mad_ms = saved.mad_ms;
        self.details.no_loop = saved.l_rt_ms.is_none();
        self.details.tendency_ms = saved.tendency_ms;
        self.details.tendency_mad_ms = saved.tendency_mad_ms;
        self.details.combined_ms = saved.combined_ms;
        self.details.combined_mad_ms = saved.combined_mad_ms;
        self.details.judgeable = saved.judgeable;
        self.details.basis = saved.basis();
        let prompt = if saved.judgeable {
            Prompt::Done
        } else {
            Prompt::RhythmLater
        };
        self.close(Outcome::Kept(saved), prompt);
    }

    /// Step 4, refused: no `L_rt`. Only the strums can give an offset (addendum A3).
    fn give_up_loop(&mut self, now: OutFrame, tap: &mut TapHandle) {
        self.l_rt = None;
        self.loop_refused = true;
        self.after_loop(now, tap);
    }

    /// The loop is settled, measured or refused: step 5 if the setting asks for it,
    /// otherwise the profile.
    fn after_loop(&mut self, now: OutFrame, tap: &mut TapHandle) {
        let strums = match self.cfg.beat_step {
            BeatStep::Always => true,
            BeatStep::WhenNoLoop => self.l_rt.is_none(),
            BeatStep::Never => false,
        };
        if strums {
            self.start_beat(now, tap);
        } else {
            self.note_loop();
            self.finish_calibrated(None, tap);
        }
    }

    /// Writes the latency in use into the details.
    fn note_loop(&mut self) {
        self.details.l_rt_ms = self.l_rt.map(|l| l.frames * 1.0e3 / self.fs_in);
        self.details.l_rt_mad_ms = self.l_rt.map(|l| l.mad * 1.0e3 / self.fs_in);
        self.details.no_loop = self.l_rt.is_none();
    }

    // -------------------------------------------------------------- beat

    fn start_beat(&mut self, now: OutFrame, tap: &mut TapHandle) {
        tap.set(false);
        self.tap_on = false;
        self.note_loop();

        let beat_s = 60.0 / self.cfg.bpm.max(1.0);
        let plan = BeatPlan {
            start: now + self.lead().max(self.out_frames(0.25)),
            period: beat_s * self.fs_out,
            count_in: self.cfg.count_in,
            beats: self.cfg.beats,
        };
        let clicks = plan.count_in + plan.beats;
        if clicks > 0 {
            let cmd = MixCmd::Metronome {
                tag: self.tag(TAG_BEAT),
                start: plan.start,
                period: plan.period,
                accent_every: 4,
                beats: clicks,
                gain: level_gain(self.cfg.click_dbfs, self.click_peak),
            };
            let due = plan.start.saturating_sub(self.send_margin());
            self.out(cmd, Some(due), OutKind::Metronome);
        }
        // The last strum may come a pairing window after its click, plus the latency the
        // strums carry, plus the time the detector takes to close the group.
        let carried = match self.l_rt {
            Some(l) => (l.frames / self.fs_in).clamp(0.0, 0.5),
            None => 0.3,
        };
        let tail = self.cfg.bias.window_frac * beat_s + carried + 0.25;
        self.beat = Some(BeatRun {
            plan,
            beats_in: Vec::with_capacity(plan.beats as usize),
            onsets: Vec::with_capacity(2 * plan.beats as usize),
            dirty: false,
            refused: clicks == 0,
            step_open: 0,
            end_at: plan.click_frame(clicks.saturating_sub(1)) + self.out_frames(tail),
        });
        self.prompt = Prompt::StrumMuted {
            count_in: plan.count_in.min(255) as u8,
            beat: 0,
            of: plan.beats.min(255) as u8,
        };
        self.phase = Phase::Beat;
    }

    fn estimate(&self, b: &BeatRun) -> Result<BiasEst, CalError> {
        let Some(l) = self.l_rt else {
            return self.estimate_alone(b);
        };
        let pb = player_bias(&b.onsets, &b.beats_in, l.frames, self.fs_in, &self.cfg.bias)?;
        Ok(BiasEst {
            center: pb.bias,
            mad: pb.mad,
            used: pb.used,
            judgeable: pb.judgeable,
            doubt: None,
        })
    }

    /// Addendum A3, no loop: the strums hold latency and tendency in one offset, read on
    /// the beat grid in `[−0.15, 0.85)` beat. A reading on a grid cannot tell an offset
    /// from the same offset plus whole beats, so it is accepted only when the strums
    /// themselves rule that out: under the offset read, they must leave fewer strums
    /// without a beat and beats without a strum than under any of the offsets one, two
    /// or three beats later (see `alias_cost`). The first scored beat is the anchor: a
    /// player who strums it leaves the later readings one strum without a beat. And the
    /// offset must be below [`CalibConfig::a3_max_offset_ms`].
    fn estimate_alone(&self, b: &BeatRun) -> Result<BiasEst, CalError> {
        let base = coarse_offset(&b.onsets, &b.beats_in);
        let pb = player_bias(&b.onsets, &b.beats_in, base, self.fs_in, &self.cfg.bias)?;
        let total = pb.bias + base;
        let period = b.plan.period * self.fs_in / self.fs_out;
        let window = self.cfg.bias.window_frac * period;
        let newest = b.onsets.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let seen_until = self.pitch_in.max(newest) - STRUM_REPORT_S * self.fs_in;
        let cost = |beats_later: f64| {
            let offset = total + beats_later * period;
            alias_cost(&b.onsets, &b.beats_in, offset, window, seen_until)
        };
        let later = [1.0, 2.0, 3.0].into_iter().map(cost).min();
        let doubt = if later.is_some_and(|c| c <= cost(0.0)) {
            Some(Warning::OffsetUnresolved)
        } else if total > self.cfg.a3_max_offset_ms * 1.0e-3 * self.fs_in {
            Some(Warning::OffsetBeyondCeiling)
        } else {
            None
        };
        Ok(BiasEst {
            center: total,
            mad: pb.mad,
            used: pb.used,
            judgeable: pb.judgeable && doubt.is_none(),
            doubt,
        })
    }

    fn advance_beat(&mut self, now: OutFrame, tap: &mut TapHandle) -> bool {
        let Some(mut b) = self.beat.take() else {
            self.phase = Phase::Done;
            return false;
        };
        let mut done = now >= b.end_at;
        if b.refused {
            self.warn(Warning::NoMetronome);
            done = true;
        } else if done && b.step_open > 0 {
            // A clock step without its unjudged window: which strums it spoiled is not
            // known, so none of them is used.
            self.beat = Some(b);
            self.warn(Warning::BeatGlitch);
            self.void_beat(now, tap);
            return true;
        }
        let mut early = false;
        if !done
            && b.step_open == 0
            && b.beats_in.len() >= self.cfg.early_stop_pairs
            && std::mem::take(&mut b.dirty)
        {
            early = self
                .estimate(&b)
                .is_ok_and(|e| e.used >= self.cfg.early_stop_pairs && e.judgeable);
        }
        if !(done || early) {
            self.beat = Some(b);
            return false;
        }
        if early {
            let tag = self.tag(TAG_BEAT);
            self.out(MixCmd::StopTag { tag, at: now }, None, OutKind::Stop);
        }
        let est = self.estimate(&b).ok();
        self.beat = Some(b);
        self.finish_calibrated(est, tap);
        true
    }

    // -------------------------------------------------------------- endings

    /// Step 6. `est` is what the strums of step 5 gave, if the step ran and enough of
    /// them were paired.
    ///
    /// A profile is judgeable only on a locked clock map: the latency and the strums
    /// were read on one, and a map that is acquiring again may no longer be that map. One
    /// that lost its lock (step 5 is long, and no probe round watches it) is given
    /// [`RELOCK_WAIT_S`] to lock again first.
    fn finish_calibrated(&mut self, est: Option<BiasEst>, tap: &mut TapHandle) {
        tap.set(false);
        self.tap_on = false;
        let acquiring = self
            .snap
            .is_some_and(|s| s.quality() == ClockQuality::Acquiring);
        if let (true, Some(now)) = (acquiring, self.now) {
            self.settling = est;
            self.prompt = Prompt::Quiet;
            self.phase = Phase::Settle {
                until: now + self.out_frames(RELOCK_WAIT_S),
            };
            return;
        }
        self.close_calibrated(est, tap);
    }

    /// The profile of step 6, on the clock map as it is now.
    ///
    /// With a loop the profile is judged with the latency alone, so the strums decide
    /// nothing: they are written down as the tendency observed. Without a loop they are
    /// the only offset there is, and they decide whether rhythm is judged at all.
    fn close_calibrated(&mut self, est: Option<BiasEst>, tap: &mut TapHandle) {
        tap.set(false);
        let to_ms = 1.0e3 / self.fs_in;
        let sound = self.locked();
        let has_loop = self.l_rt.is_some();
        let judgeable = sound && (has_loop || est.as_ref().is_some_and(|e| e.judgeable));
        self.details.strums_used = est.as_ref().map_or(0, |e| e.used as u32);
        self.details.tendency_ms = None;
        self.details.tendency_mad_ms = None;
        self.details.combined_ms = None;
        self.details.combined_mad_ms = None;
        let mut combined_n = None;
        match est.as_ref() {
            Some(e) if has_loop => {
                self.details.tendency_ms = Some(e.center * to_ms);
                self.details.tendency_mad_ms = Some(e.mad * to_ms);
            }
            Some(e) => match e.doubt {
                // The offset is not known: no number that something could judge with.
                Some(doubt) => {
                    self.warn(doubt);
                    self.details.unresolved_offset_ms = Some(e.center * to_ms);
                }
                None => {
                    self.details.combined_ms = Some(e.center * to_ms);
                    self.details.combined_mad_ms = Some(e.mad * to_ms);
                    combined_n = Some(e.used as u32);
                }
            },
            None => {}
        }
        self.details.judgeable = judgeable;
        // 0.1 µs and 0.001 ppm: far below what is measured, and exact in the file.
        let ms = |x: Option<f64>| x.map(|v| rounded(v, 1.0e4));
        let profile = Profile {
            key: self.cfg.key.clone(),
            l_rt_ms: ms(self.details.l_rt_ms),
            mad_ms: ms(self.details.l_rt_mad_ms),
            tendency_ms: ms(self.details.tendency_ms),
            tendency_mad_ms: ms(self.details.tendency_mad_ms),
            combined_ms: ms(self.details.combined_ms),
            combined_mad_ms: ms(self.details.combined_mad_ms),
            combined_n,
            judgeable,
            date: self.cfg.date.clone(),
            clock_quality: self.snap.map_or(ClockQuality::Acquiring, |s| s.quality()),
            drift_ppm: rounded(self.snap.map_or(0.0, |s| s.drift_ppm()), 1.0e3),
            stamps_in: self.src.map_or(StampSource::Device, |s| s.0),
            stamps_out: self.src.map_or(StampSource::Device, |s| s.1),
        };
        self.details.basis = profile.basis();
        let prompt = if judgeable {
            Prompt::Done
        } else {
            Prompt::RhythmLater
        };
        self.close(Outcome::Calibrated(profile), prompt);
    }

    fn close(&mut self, outcome: Outcome, prompt: Prompt) {
        self.tap_on = false;
        self.round = None;
        // Only the stops still have to reach the mixer, if there is one.
        self.outbox
            .retain(|o| o.what == OutKind::Stop && !self.dead);
        if let Some(s) = self.snap {
            self.details.clock_quality = s.quality();
            self.details.drift_ppm = s.drift_ppm();
            self.details.stamps_in = s.source(Side::In);
            self.details.stamps_out = s.source(Side::Out);
        }
        if let Some(now) = self.now {
            self.details.seconds = (now - self.t0) as f64 / self.fs_out;
        }
        self.outcome = Some(outcome);
        self.prompt = prompt;
        self.phase = Phase::Done;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_level_passes_minus_six_dbfs() {
        let limit = 10f64.powf(MAX_DBFS / 20.0);
        for fs in [44_100.0, 48_000.0] {
            let bank = SoundBank::new(fs);
            for id in [SoundId::Probe, SoundId::Click, SoundId::Accent] {
                let peak = true_peak(bank.get(id));
                assert!(
                    peak >= peak_of(bank.get(id)) && peak < 1.0,
                    "{id:?}: {peak}"
                );
                for dbfs in [-30.0, -12.0, -8.0, -6.0, -3.0, 0.0, 12.0] {
                    let out = f64::from(level_gain(dbfs, peak) * peak);
                    assert!(out <= limit, "{id:?} at {dbfs} dBFS plays at {out}");
                    if dbfs <= MAX_DBFS {
                        let want = 10f64.powf(dbfs / 20.0);
                        assert!((out / want - 1.0).abs() < 1.0e-4, "{id:?} {dbfs}: {out}");
                    }
                }
            }
        }
        assert_eq!(level_gain(-12.0, 0.0), 0.0);
        assert_eq!(level_gain(f64::NAN, 0.4), 0.0);
    }

    #[test]
    fn true_peak_sees_between_the_samples() {
        // A quarter-rate sine sampled 45° off its crests: every sample is ±1/√2, the
        // signal reaches 1.
        let burst: Vec<f32> = (0..400)
            .map(|i| {
                let fade = (PI * (f64::from(i) + 0.5) / 400.0).sin().powi(2).min(0.2) / 0.2;
                (fade * (PI * (0.5 * f64::from(i) + 0.25)).sin()) as f32
            })
            .collect();
        let (samples, real) = (peak_of(&burst), true_peak(&burst));
        assert!(
            (samples - std::f32::consts::FRAC_1_SQRT_2).abs() < 1.0e-3,
            "{samples}"
        );
        assert!((1.0..1.03).contains(&real), "{real}");
        // Slow content: the samples already carry the peak (plus the grid margin).
        let slow: Vec<f32> = (0..400)
            .map(|i| (PI * f64::from(i) / 100.0).sin() as f32)
            .collect();
        assert!(
            (1.0..1.021).contains(&true_peak(&slow)),
            "{}",
            true_peak(&slow)
        );
        assert_eq!(true_peak(&[]), 0.0);
    }

    #[test]
    fn robust_line_ignores_a_transient() {
        // g = 100 + 0.91875·f with small noise; five early points 40 frames off.
        let mut rng = Mulberry32::new(5);
        let mut pts: Vec<(f64, f64)> = (0..50)
            .map(|k| {
                let f = 1.0e9 + 4800.0 * f64::from(k);
                let noise = (rng.next_f64() - 0.5) * 0.4;
                (f, 100.0 + 0.918_75 * (f - 1.0e9) + noise)
            })
            .collect();
        for p in pts.iter_mut().take(5) {
            p.1 += 40.0;
        }
        let plain = least_squares(&pts).unwrap();
        let robust = robust_line(&pts).unwrap();
        let truth = |f: f64| 100.0 + 0.918_75 * (f - 1.0e9);
        let f = 1.0e9 + 200_000.0;
        assert!(
            (plain.at(f) - truth(f)).abs() > 1.0,
            "the control must be off"
        );
        assert!((robust.at(f) - truth(f)).abs() < 0.1);
        assert!((robust.slope - 0.918_75).abs() < 1.0e-6);
        assert!(least_squares(&pts[..1]).is_none());
        assert!(robust_line(&[(1.0, 2.0), (1.0, 3.0)]).is_none());
    }

    #[test]
    fn coarse_offset_reads_the_whole_latency() {
        let period = 36_000.0;
        let beats: Vec<f64> = (0..16).map(|k| 1.0e6 + period * f64::from(k)).collect();
        let mut rng = Mulberry32::new(9);
        for want in [-3_000.0, 0.0, 8_000.0, 14_400.0, 24_000.0] {
            let onsets: Vec<f64> = beats
                .iter()
                .map(|b| b + want + (rng.next_f64() - 0.5) * 1_500.0)
                .collect();
            let got = coarse_offset(&onsets, &beats);
            assert!((got - want).abs() < 600.0, "want {want}, got {got}");
        }
        assert_eq!(coarse_offset(&[], &beats), 0.0);
        assert_eq!(coarse_offset(&[1.0], &beats[..1]), 0.0);
    }

    #[test]
    fn alias_cost_tells_a_reading_from_the_same_reading_a_beat_later() {
        // 80 BPM at 48 kHz, 16 beats, pairing window of 0.35 beat.
        let period = 36_000.0;
        let window = 0.35 * period;
        let beats: Vec<f64> = (0..16).map(|k| 1.0e6 + period * f64::from(k)).collect();
        let last = beats[15];
        let cost = |onsets: &[f64], offset: f64, seen: f64| {
            alias_cost(onsets, &beats, offset, window, seen)
        };
        // What step 5 has seen when it ends, and a full beat more than that.
        let (short, long) = (last + 0.6 * 48_000.0, last + 1.6 * 48_000.0);

        // Every beat strummed 53 ms late.
        let r = 2_544.0;
        let all: Vec<f64> = beats.iter().map(|b| b + r).collect();
        assert_eq!(cost(&all, r, short), 0);
        // One beat more: the first strum has no beat; the last beat has no strum, which
        // counts once its time has passed.
        assert_eq!(cost(&all, r + period, short), 1);
        assert_eq!(cost(&all, r + period, long), 2);
        assert_eq!(cost(&all, r + 2.0 * period, short), 2);

        // Every beat strummed 700 ms late, read as 50 ms early: the first beat has no
        // strum and the last strum has no beat. One beat more fits all of them.
        let slow: Vec<f64> = beats.iter().map(|b| b + 33_600.0).collect();
        assert_eq!(cost(&slow[..15], -2_400.0, short), 1);
        assert_eq!(cost(&slow, -2_400.0, long), 2);
        assert_eq!(cost(&slow[..15], 33_600.0, short), 0);
        assert_eq!(cost(&slow, 33_600.0, long), 0);

        // A late start: no better than its alias, whatever was seen.
        assert_eq!(cost(&all[1..], r, short), 1);
        assert_eq!(cost(&all[1..], r + period, short), 0);
        assert_eq!(cost(&all[1..], r + period, long), 1);

        // A missed beat in the middle and a double strum cost both readings the same.
        let mut missed = all.clone();
        missed.remove(7);
        assert_eq!(
            (cost(&missed, r, long), cost(&missed, r + period, long)),
            (1, 3)
        );
        let mut double = all.clone();
        double.insert(6, beats[5] + r + 3_000.0);
        assert_eq!(
            (cost(&double, r, long), cost(&double, r + period, long)),
            (1, 3)
        );

        // Strums on the count-in answer no scored beat under either reading.
        let mut counted: Vec<f64> = (1..=4)
            .map(|k| beats[0] - period * f64::from(k) + r)
            .collect();
        counted.extend(&all);
        assert_eq!(
            (cost(&counted, r, long), cost(&counted, r + period, long)),
            (4, 6)
        );

        // Nothing to pair.
        assert_eq!(cost(&[], r, long), 16);
        assert_eq!(alias_cost(&all, &[], r, window, long), 16);
    }

    #[test]
    fn level_counts_zeros_and_clipping() {
        let mut l = Level::default();
        l.add(&[0.0; 100]);
        assert_eq!((l.nonzero, l.floor_dbfs(), l.peak_dbfs()), (0, None, None));
        l.add(&[0.5, -0.99, 1.0, 0.1]);
        assert_eq!((l.n, l.nonzero, l.clipped), (104, 4, 2));
        assert!((l.peak_dbfs().unwrap()).abs() < 1.0e-9);
        assert!((l.clip_fraction() - 2.0 / 104.0).abs() < 1.0e-12);
    }

    #[test]
    fn short_round_gaps_are_spread_and_shuffled() {
        let sorted = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v
        };
        let mut orders = Vec::new();
        for seed in 0..40 {
            let gaps = spread_gaps(5, 0.30, 0.04, &mut Mulberry32::new(seed));
            // The mean is kept exactly, so the round lasts the same whatever the order.
            assert!((gaps.iter().sum::<f64>() - 1.5).abs() < 1.0e-12);
            let s = sorted(gaps.clone());
            for (x, want) in s.iter().zip([0.26, 0.28, 0.30, 0.32, 0.34]) {
                assert!((x - want).abs() < 1.0e-12, "{s:?}");
            }
            if !orders.contains(&gaps) {
                orders.push(gaps);
            }
        }
        assert!(orders.len() > 20, "{} orders in 40 seeds", orders.len());

        assert!(spread_gaps(0, 0.3, 0.04, &mut Mulberry32::new(1)).is_empty());
        assert_eq!(spread_gaps(1, 0.3, 0.04, &mut Mulberry32::new(1)), [0.3]);
        let two = sorted(spread_gaps(2, 0.3, 0.04, &mut Mulberry32::new(1)));
        assert!((two[0] - 0.26).abs() < 1.0e-12 && (two[1] - 0.34).abs() < 1.0e-12);
        // No spread asked for: equal gaps.
        assert_eq!(
            spread_gaps(3, 0.3, 0.0, &mut Mulberry32::new(1)),
            [0.3, 0.3, 0.3]
        );
        // The gaps of the short configuration are far above the floor `start_round`
        // puts under every gap, which would make them equal again.
        let cfg = CalibConfig::short(
            ProfileKey {
                host: String::new(),
                in_id: String::new(),
                out_id: String::new(),
                fs_in: 48_000,
                fs_out: 48_000,
            },
            "",
        );
        assert!(cfg.short_gap_s - cfg.short_gap_jitter_s > 4.0 * PROBE_S);
        assert_eq!((cfg.short_probes, cfg.short_min_found), (6, 5));
        assert_eq!((cfg.short_gap_s, cfg.short_gap_jitter_s), (0.30, 0.04));
        assert_eq!((cfg.silence_s, cfg.beat_step), (1.0, BeatStep::WhenNoLoop));
    }

    /// What the windows of a round read, in ms after the start of each window, and those
    /// starts. Probe `k` arrives `delays[k]` ms after its emission (`None`: never); a
    /// window starts `pre` before the emission of its probe and is `pre + search` long.
    /// `pick` chooses among the arrivals a window holds, earliest first: the matched
    /// filter takes the first that is at least half as strong as the strongest, and
    /// which one that is depends on levels this model does not have.
    fn model_round(
        gaps: &[f64],
        delays: &[Option<f64>],
        (pre, search): (f64, f64),
        mut pick: impl FnMut(usize) -> usize,
    ) -> (Vec<Option<f64>>, Vec<f64>) {
        let mut emit = vec![0.0];
        for g in gaps {
            emit.push(emit[emit.len() - 1] + g);
        }
        let starts: Vec<f64> = emit.iter().map(|e| e - pre).collect();
        let lags = starts
            .iter()
            .map(|start| {
                let mut held: Vec<f64> = emit
                    .iter()
                    .zip(delays)
                    .filter_map(|(e, d)| d.map(|d| e + d - start))
                    .filter(|lag| (0.0..=pre + search).contains(lag))
                    .collect();
                held.sort_by(f64::total_cmp);
                (!held.is_empty()).then(|| held[pick(held.len()).min(held.len() - 1)])
            })
            .collect();
        (lags, starts)
    }

    /// The rule the short round had at first: 5 readings with a MAD within 3 ms. The
    /// latency it reports, ms after the window start, if it accepts.
    fn agreement_alone(lags: &[Option<f64>]) -> Option<f64> {
        let found: Vec<f64> = lags.iter().flatten().copied().collect();
        let (median, mad) = median_mad(&found);
        (found.len() >= 5 && mad <= 3.0).then_some(median)
    }

    /// Review of A2, finding 1. Whatever the latency does from one probe to the next,
    /// and whichever arrival a window takes, a reading the short round counts is the
    /// arrival of the probe it was searched for.
    #[test]
    fn short_round_counts_only_readings_of_the_right_probe() {
        let cfg = CalibConfig::short(
            ProfileKey {
                host: String::new(),
                in_id: String::new(),
                out_id: String::new(),
                fs_in: 48_000,
                fs_out: 48_000,
            },
            "",
        );
        let window = (cfg.pre_roll_s * 1.0e3, cfg.loopback.search_max_s * 1.0e3);
        let (pre, tol) = (window.0, cfg.loopback.max_mad_ms);
        let needed = cfg.short_min_found as usize;
        assert_eq!((window, tol, needed), ((50.0, 500.0), 3.0, 5));
        let first = |_: usize| 0;
        // The latency the round reports, ms, and the probes it counts.
        let judge = |lags: &[Option<f64>], starts: &[f64]| {
            let own = vouched(lags, starts, tol);
            (own.len() >= needed).then(|| (median_mad(&own).0 - pre, own.len()))
        };

        // The counter-example of the review, with the gaps of the default seed: every
        // probe but the last arrives 10 ms after the next one is emitted.
        let gaps = [280.0, 340.0, 320.0, 300.0, 260.0];
        let follows: Vec<Option<f64>> = [290.0, 350.0, 330.0, 310.0, 270.0, 300.0]
            .map(Some)
            .to_vec();
        let (lags, starts) = model_round(&gaps, &follows, window, first);
        let want = [340.0, 60.0, 60.0, 60.0, 60.0, 60.0].map(Some);
        assert_eq!(lags, want);
        // Agreement alone reports 10 ms, which no probe had.
        assert_eq!(agreement_alone(&lags), Some(60.0));
        assert_eq!(judge(&lags, &starts), None);
        // The same trick behind a first probe that is read as itself: the second probe
        // is the late one, and the readings after it are its and its successors'.
        let behind = [10.0, 350.0, 330.0, 310.0, 270.0, 300.0].map(Some);
        let (lags, starts) = model_round(&gaps, &behind, window, first);
        assert_eq!(lags, [60.0, 400.0, 60.0, 60.0, 60.0, 60.0].map(Some));
        assert_eq!(agreement_alone(&lags), Some(60.0));
        assert_eq!(judge(&lags, &starts), None);
        assert_eq!(vouched(&lags, &starts, tol), [60.0]);

        // What is accepted: a steady latency, heard by all, or by all but the last.
        let steady = |ms: f64| [Some(ms); 6];
        let (lags, starts) = model_round(&gaps, &steady(87.3), window, first);
        let (read, count) = judge(&lags, &starts).expect("a steady latency is read");
        assert!((read - 87.3).abs() < 1.0e-9 && count == 6);
        let mut last_lost = steady(87.3);
        last_lost[5] = None;
        let (lags, starts) = model_round(&gaps, &last_lost, window, first);
        assert_eq!(judge(&lags, &starts).map(|x| x.1), Some(5));
        // A probe lost in the middle: the ones after it cannot be vouched for (the
        // lost one may have arrived in their windows), so the full round decides.
        for lost in 0..5 {
            let mut delays = steady(87.3);
            delays[lost] = None;
            let (lags, starts) = model_round(&gaps, &delays, window, first);
            assert_eq!(judge(&lags, &starts), None, "probe {lost} lost");
            assert_eq!(vouched(&lags, &starts, tol).len(), lost);
        }
        // Up to where a reading could be the next probe: 260 ms between two probes,
        // less the pre-roll and twice the tolerance.
        for (ms, read) in [(-40.0, true), (0.0, true), (203.0, true), (205.0, false)] {
            let (lags, starts) = model_round(&gaps, &steady(ms), window, first);
            assert_eq!(judge(&lags, &starts).is_some(), read, "{ms} ms");
        }
        for ms in [230.0, 250.0, 290.0, 400.0, 520.0] {
            let (lags, starts) = model_round(&gaps, &steady(ms), window, first);
            assert_eq!(judge(&lags, &starts), None, "{ms} ms");
        }
        // Nothing to count.
        assert!(vouched(&[], &[], tol).is_empty());
        assert!(vouched(&[None, Some(60.0)], &[0.0, 300.0], tol).is_empty());
        assert!(vouched(&[Some(60.0)], &[0.0, 300.0], tol).is_empty());
        assert_eq!(vouched(&[Some(60.0)], &[0.0], tol), [60.0]);

        // Delays that vary independently from probe to probe, in every mix of: a
        // common latency with a little jitter, a delay that follows the gap or the two
        // gaps after the probe (the two ways to land where a later probe is expected),
        // anything at all, and never. Gaps in every order the seed can give.
        let mut rng = Mulberry32::new(0x5017);
        let mut pick_rng = Mulberry32::new(7);
        let (mut rounds, mut accepted, mut fooled_before) = (0u32, 0u32, 0u32);
        for case in 0..150_000u32 {
            let gaps: Vec<f64> = spread_gaps(5, cfg.short_gap_s, cfg.short_gap_jitter_s, &mut rng)
                .iter()
                .map(|g| (g * 1.0e3).round())
                .collect();
            let base = -40.0 + 260.0 * rng.next_f64();
            let wild = rng.next_f64();
            let delays: Vec<Option<f64>> = (0..6)
                .map(|k| {
                    let next = gaps.get(k).copied().unwrap_or(300.0);
                    let after = gaps.get(k + 1).copied().unwrap_or(300.0);
                    let jitter = 2.0 * rng.next_f64() - 1.0;
                    let style = rng.next_f64();
                    if style > wild {
                        Some(base + jitter)
                    } else if style < 0.35 * wild {
                        Some(base + next + jitter)
                    } else if style < 0.5 * wild {
                        Some(base + next + after + jitter)
                    } else if style < 0.8 * wild {
                        Some(-50.0 + 950.0 * rng.next_f64())
                    } else {
                        None
                    }
                })
                .collect();
            // The first arrival in every window, then any arrival.
            for any in [false, true] {
                let (lags, starts) = model_round(&gaps, &delays, window, |held| {
                    if any {
                        (pick_rng.next_f64() * held as f64) as usize
                    } else {
                        0
                    }
                });
                rounds += 1;
                let own = vouched(&lags, &starts, tol);
                for (k, lag) in own.iter().enumerate() {
                    let delay = delays[k].unwrap_or(f64::NAN);
                    assert!(
                        (lag - (delay + pre)).abs() < 1.0e-9,
                        "case {case}: gaps {gaps:?}, delays {delays:?}, read {lags:?}: probe {k}"
                    );
                }
                if let Some((read, count)) = judge(&lags, &starts) {
                    accepted += 1;
                    // Each within the tolerance of the first, so of the median twice.
                    for delay in &delays[..count] {
                        assert!((delay.unwrap() - delays[0].unwrap()).abs() <= tol + 1.0e-9);
                        assert!((delay.unwrap() - read).abs() <= 2.0 * tol, "case {case}");
                    }
                }
                // Agreement alone, for comparison: a latency that at most two of the
                // probes had.
                if let Some(median) = agreement_alone(&lags) {
                    let had = delays
                        .iter()
                        .flatten()
                        .filter(|d| (*d + pre - median).abs() <= tol)
                        .count();
                    fooled_before += u32::from(had <= 2);
                }
            }
        }
        assert_eq!(rounds, 300_000);
        // The rule is not vacuous, and the hazard is not a single hand-made case.
        assert!(accepted > 10_000, "{accepted} of {rounds} accepted");
        assert!(
            fooled_before > 100,
            "{fooled_before} rounds fooled agreement alone"
        );
        eprintln!(
            "{accepted} of {rounds} short rounds accepted; agreement alone: {fooled_before} wrong"
        );
    }
}
