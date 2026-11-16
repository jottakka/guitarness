//! Audio bridge and song clock (unit G1).
//!
//! The game never blocks on audio and never touches a device itself. This module owns the
//! audio engine (`NonSend` resource [`AudioRig`]) and the session (`guitarness_session`), and
//! gives the rest of the game four things:
//!
//! - [`SongClock`]: written in `PreUpdate` from the *audio* clock, never from frame time:
//!   `t = (out_frame - anchor) / fs_out - video_offset`, where `out_frame` is the output
//!   frame sounding now ([`PlayClock`]) and `anchor` is chart time zero from the session's
//!   `RoundArmed`. With no armed round the clock rests at [`LEAD_IN`]. While the streams
//!   are closed (device lost) it holds. It never goes back inside one epoch.
//! - [`AudioCommand`] messages in (soundcheck, start/stop round, ...), [`AudioNote`]
//!   messages out (every `SessionEvent`), [`AudioLink`] messages for the device state.
//! - [`AudioStatus`]: a plain snapshot of the bridge for drawing.
//! - The reopen policy: after a device loss, up to [`REOPEN_TRIES`] tries, [`REOPEN_GAP_S`]
//!   apart; then the link rests in [`Link::Gone`] until [`AudioCommand::Retry`] (key R).
//!
//! # Threads
//!
//! `Fake`: engine in `Pump::Manual`, session inline; the system advances virtual time by the
//! frame time in 5 ms slices (engine step, then `Session::step`), so tests are deterministic.
//! `Live`: engine in `Pump::Thread`, session on its own thread; the frame only reads the
//! playhead and drains a channel. The real backend is never opened when
//! [`real_devices_allowed`] is false.
//!
//! # Command line (read from `AppConfig::args`)
//!
//! `--fake`, `--live`, `--player tend_ms,sigma_ms|none`, `--replay-wav <file>`, `--in <id>`,
//! `--out <id>`, `--data <dir>`, `--no-tape`, `--tape`, `--video-offset <ms>`. `--in` or
//! `--out` imply `--live`. Without any of them the mode is [`AudioMode::None`] and the frame
//! clock of the prototype runs.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bevy::prelude::*;

use guitarness_audio::calib::Basis;
use guitarness_audio::playhead::{PlayClock, PlayMode, Playhead};
use guitarness_audio::{
    AudioEngine, AudioError, CpalBackend, EngineConfig, FakeConfig, FakeHandle, MixerHandle,
    OpenRequest, Pump, SoundBank, StreamInfo, TapHandle,
};
use guitarness_core::chart::Chart as CoreChart;
use guitarness_core::timing::Level;
use guitarness_session::rig::{ChartHit, FakeRig, HitKind, PlayerModel, real_devices_allowed};
use guitarness_session::session::{
    Session, SessionCmd, SessionConfig, SessionEvent, SessionPort, SessionThread, SoundOutcome,
    TAPES_DEFAULT_ON, default_profile_path,
};

use crate::gfx::SongClock;
use crate::{AppConfig, LEAD_IN};

/// Virtual time per step of the fake rig (the S2 rhythm: engine step, then session step).
pub const SLICE_S: f64 = 0.005;
/// A frame longer than this is treated as this long (a window drag must not fast-forward).
pub const MAX_FRAME_S: f64 = 0.25;
/// Tries to reopen the device after a loss.
pub const REOPEN_TRIES: u8 = 3;
/// Seconds before each try.
pub const REOPEN_GAP_S: f64 = 1.0;

/// Where the song clock comes from. `None` keeps the frame clock of the prototype
/// (gallery, screenshots).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AudioMode {
    #[default]
    None,
    /// The fake backend in virtual time (`--fake`).
    Fake,
    /// The real devices (cpal).
    Live,
}

/// State of the device link, for the calm "plug" screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Link {
    /// No bridge (mode `None`).
    #[default]
    Off,
    /// Streams open.
    Up,
    /// The device stopped; try number `attempt` (1-based) is next or running.
    Reopening { attempt: u8 },
    /// Every try failed: show the plug pictogram, wait for [`AudioCommand::Retry`].
    Gone,
    /// The devices could not be opened at all (or are forbidden).
    Unavailable,
}

/// Seconds the picture trails the sound (screen latency), subtracted from the song clock.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct VideoOffset(pub f64);

/// What the bridge knows, for drawing. Written by the bridge only.
#[derive(Resource, Clone, Debug, Default)]
pub struct AudioStatus {
    pub link: Link,
    /// Engine epoch (grows by one at every reopen).
    pub epoch: u32,
    pub fs_out: u32,
    /// Output frame sounding now (fractional), as the play clock gives it.
    pub out_frame: f64,
    /// Where `out_frame` came from; `None` before the first position.
    pub play_mode: Option<PlayMode>,
    /// Chart time zero of the armed round, output frame.
    pub anchor: Option<u64>,
    /// A round is armed and not done.
    pub round_active: bool,
    /// Basis of the last soundcheck.
    pub basis: Option<Basis>,
    /// The engine says the calibration no longer applies (a reopen happened).
    pub calibration_stale: bool,
    /// A short reason when the link is `Unavailable`.
    pub notice: Option<String>,
}

/// What the game asks of the audio side. Sent by G2 (soundcheck), G3 (round), G4 (record).
#[derive(Message, Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum AudioCommand {
    Soundcheck {
        force: bool,
    },
    StartRound {
        chart: CoreChart,
        lead_in_beats: u32,
        take: Option<String>,
    },
    StopRound,
    Dispute,
    SetLevel(Level),
    Record(bool),
    /// Stop a running soundcheck; the session keeps the previous basis.
    CancelSoundcheck,
    /// Try to open the device again after [`Link::Gone`].
    Retry,
}

/// Every event of the session, in order.
#[derive(Message, Clone, Debug)]
pub struct AudioNote(pub SessionEvent);

/// Device link transitions (the matching state is in [`AudioStatus::link`]).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioLink {
    /// The device stopped; the round (if any) is over.
    Lost,
    /// A reopen try starts.
    Retrying { attempt: u8 },
    /// The streams are open again in `epoch`; the session was rewired.
    Back { epoch: u32 },
    /// Every try failed.
    Gone,
    /// The devices cannot be opened.
    Unavailable,
}

/// The bridge runs in this set, in `PreUpdate`. Order your systems after it.
#[derive(SystemSet, Clone, Debug, Hash, PartialEq, Eq)]
pub struct AudioSet;

// ------------------------------------------------------------------ reopen policy

/// The reopen schedule, free of time sources: the caller feeds it frame time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReopenPolicy {
    phase: Phase,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    Waiting { left_s: f64, attempt: u8 },
    Trying { attempt: u8 },
    Gone,
}

impl Default for ReopenPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl ReopenPolicy {
    pub fn new() -> Self {
        Self { phase: Phase::Idle }
    }

    /// The device stopped: the first try comes after [`REOPEN_GAP_S`]. No-op if already running.
    pub fn start(&mut self) {
        if self.phase == Phase::Idle {
            self.phase = Phase::Waiting {
                left_s: REOPEN_GAP_S,
                attempt: 1,
            };
        }
    }

    /// Moves time forward; `Some(n)` means "try number `n` now" (answer with `ok` or `failed`).
    pub fn tick(&mut self, dt_s: f64) -> Option<u8> {
        if let Phase::Waiting { left_s, attempt } = self.phase {
            let left_s = left_s - dt_s;
            if left_s <= 0.0 {
                self.phase = Phase::Trying { attempt };
                return Some(attempt);
            }
            self.phase = Phase::Waiting { left_s, attempt };
        }
        None
    }

    /// The try worked.
    pub fn ok(&mut self) {
        self.phase = Phase::Idle;
    }

    /// The try failed; returns true when no tries are left (the link is gone).
    pub fn failed(&mut self) -> bool {
        match self.phase {
            Phase::Trying { attempt } if attempt < REOPEN_TRIES => {
                self.phase = Phase::Waiting {
                    left_s: REOPEN_GAP_S,
                    attempt: attempt + 1,
                };
                false
            }
            Phase::Trying { .. } => {
                self.phase = Phase::Gone;
                true
            }
            _ => false,
        }
    }

    /// The player asked again after `Gone`: a fresh round of tries, the first at once.
    pub fn retry(&mut self) {
        if self.phase == Phase::Gone {
            self.phase = Phase::Waiting {
                left_s: 0.0,
                attempt: 1,
            };
        }
    }

    pub fn is_gone(&self) -> bool {
        self.phase == Phase::Gone
    }

    pub fn is_idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    /// The try number waiting or running, if any.
    pub fn attempt(&self) -> Option<u8> {
        match self.phase {
            Phase::Waiting { attempt, .. } | Phase::Trying { attempt } => Some(attempt),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ options and args

/// Which engine the bridge opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Fake,
    Live,
}

/// Everything the command line can say about the audio.
#[derive(Clone, Debug)]
pub struct BridgeOptions {
    /// Virtual player of the fake rig (`None`: nobody plays).
    pub player: Option<PlayerModel>,
    /// A WAV file as the microphone (fake only; replaces the player).
    pub replay_wav: Option<PathBuf>,
    pub in_id: Option<String>,
    pub out_id: Option<String>,
    /// Folder of `profiles.json` and `tapes/`.
    pub data: Option<PathBuf>,
    /// `Some(true)` records tapes (needs `data`), `Some(false)` never; `None`: the default.
    pub tape: Option<bool>,
    /// Screen latency, ms.
    pub video_offset_ms: f64,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            player: Some(PlayerModel::default()),
            replay_wav: None,
            in_id: None,
            out_id: None,
            data: None,
            tape: None,
            video_offset_ms: 0.0,
        }
    }
}

/// What `parse_args` found.
#[derive(Clone, Debug, Default)]
pub struct BridgeArgs {
    pub source: Option<Source>,
    pub opts: BridgeOptions,
    pub warnings: Vec<String>,
}

/// Reads the audio flags out of `AppConfig::args` (unknown words are ignored).
pub fn parse_args(args: &[String]) -> BridgeArgs {
    let mut out = BridgeArgs::default();
    let mut fake = false;
    let mut live = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--fake" => fake = true,
            "--live" => live = true,
            "--no-tape" => out.opts.tape = Some(false),
            "--tape" => out.opts.tape = Some(true),
            "--player" => match it.next().map(String::as_str) {
                Some("none") => out.opts.player = None,
                Some(v) => match parse_pair(v) {
                    Some((tendency_ms, sigma_ms)) => {
                        out.opts.player = Some(PlayerModel {
                            tendency_ms,
                            sigma_ms,
                            ..PlayerModel::default()
                        });
                    }
                    None => out.warnings.push(format!(
                        "--player expects tend_ms,sigma_ms or none, got {v:?}"
                    )),
                },
                None => out.warnings.push("--player expects a value".into()),
            },
            "--video-offset" => match it.next().and_then(|v| v.parse::<f64>().ok()) {
                Some(ms) if ms.is_finite() => out.opts.video_offset_ms = ms,
                _ => out
                    .warnings
                    .push("--video-offset expects milliseconds".into()),
            },
            "--replay-wav" => out.opts.replay_wav = it.next().map(PathBuf::from),
            "--data" => out.opts.data = it.next().map(PathBuf::from),
            "--in" => {
                out.opts.in_id = it.next().cloned();
                live = true;
            }
            "--out" => {
                out.opts.out_id = it.next().cloned();
                live = true;
            }
            _ => {}
        }
    }
    out.source = if fake {
        Some(Source::Fake)
    } else if live {
        Some(Source::Live)
    } else {
        None
    };
    out
}

fn parse_pair(v: &str) -> Option<(f64, f64)> {
    let (a, b) = v.split_once(',')?;
    let (a, b) = (a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?);
    (a.is_finite() && b.is_finite() && b >= 0.0).then_some((a, b))
}

// ------------------------------------------------------------------ the rig

enum Engine {
    Fake(Box<FakeRig>),
    Live(Box<AudioEngine<CpalBackend>>),
}

enum Driver {
    Inline(Box<Session>),
    Thread(#[allow(dead_code)] SessionThread),
}

/// The audio engine and its session. A `NonSend` resource: the engine's streams belong to the
/// thread that built them.
pub struct AudioRig {
    engine: Engine,
    driver: Driver,
    port: SessionPort,
    play: PlayClock,
    playhead: Playhead,
    policy: ReopenPolicy,
    player: Option<PlayerModel>,
    /// The chart of the round asked for, for the virtual player.
    round_chart: Option<CoreChart>,
    cal_scheduled: bool,
    carry_s: f64,
    /// Chart time zero (output frame) and the epoch it belongs to.
    anchor: Option<(u64, u32)>,
    round_active: bool,
    basis: Option<Basis>,
    lost: bool,
    out: Vec<SessionEvent>,
    seen: usize,
}

impl AudioRig {
    /// A rig on the fake backend, session inline. `ec` tunes the engine.
    pub fn fake(fc: FakeConfig, opts: &BridgeOptions) -> Result<AudioRig, AudioError> {
        let fs = fc.fs_out;
        let fs_in = fc.fs_in;
        let mut rig = FakeRig::with_engine(fc, EngineConfig::default());
        if let Some(path) = &opts.replay_wav {
            rig.feed_wav(path)
                .map_err(|e| AudioError::Backend(format!("--replay-wav: {e}")))?;
        }
        // The session owns the client side of the engine; the rig keeps the rest.
        let io = std::mem::replace(&mut rig.io, FakeRig::new(FakeConfig::new(fs_in, fs)).io);
        let info = rig.engine.info().clone();
        let (session, port) = Session::new(io, info, session_config("fake", opts));
        let playhead = rig.engine.playhead();
        let play = PlayClock::new(f64::from(fs));
        Ok(AudioRig {
            engine: Engine::Fake(Box::new(rig)),
            driver: Driver::Inline(Box::new(session)),
            port,
            play,
            playhead,
            policy: ReopenPolicy::new(),
            player: if opts.replay_wav.is_some() {
                None
            } else {
                opts.player
            },
            round_chart: None,
            cal_scheduled: false,
            carry_s: 0.0,
            anchor: None,
            round_active: false,
            basis: None,
            lost: false,
            out: Vec::new(),
            seen: 0,
        })
    }

    /// A rig on the real devices, session on its own thread. Refuses when real devices are
    /// forbidden (`GUITARNESS_FORBID_DEVICES`).
    pub fn live(opts: &BridgeOptions) -> Result<AudioRig, AudioError> {
        if !real_devices_allowed() {
            return Err(AudioError::NoDevice(
                "real audio devices are forbidden in this run".into(),
            ));
        }
        let ec = EngineConfig {
            open: OpenRequest {
                input_id: opts.in_id.clone(),
                output_id: opts.out_id.clone(),
                buffer_frames: None,
            },
            ..EngineConfig::default()
        };
        let (engine, io) = AudioEngine::start(
            CpalBackend::new(),
            ec,
            SoundBank::new(48_000.0),
            Pump::Thread,
        )?;
        let info = engine.info().clone();
        let fs = f64::from(info.fs_out);
        let (session, port) = Session::new(io, info, session_config(host_name(), opts));
        let thread = session.spawn(Duration::from_millis(5));
        let playhead = engine.playhead();
        Ok(AudioRig {
            engine: Engine::Live(Box::new(engine)),
            driver: Driver::Thread(thread),
            port,
            play: PlayClock::new(fs),
            playhead,
            policy: ReopenPolicy::new(),
            player: None,
            round_chart: None,
            cal_scheduled: false,
            carry_s: 0.0,
            anchor: None,
            round_active: false,
            basis: None,
            lost: false,
            out: Vec::new(),
            seen: 0,
        })
    }

    // -------------------------------------------------------------- accessors

    pub fn mode(&self) -> AudioMode {
        match self.engine {
            Engine::Fake(_) => AudioMode::Fake,
            Engine::Live(_) => AudioMode::Live,
        }
    }

    /// Control of the fake devices (virtual clock, truth, injected faults).
    pub fn fake_handle(&self) -> Option<FakeHandle> {
        match &self.engine {
            Engine::Fake(r) => Some(r.handle.clone()),
            Engine::Live(_) => None,
        }
    }

    pub fn epoch(&self) -> u32 {
        match &self.engine {
            Engine::Fake(r) => r.engine.epoch(),
            Engine::Live(e) => e.epoch(),
        }
    }

    pub fn info(&self) -> StreamInfo {
        match &self.engine {
            Engine::Fake(r) => r.engine.info().clone(),
            Engine::Live(e) => e.info().clone(),
        }
    }

    pub fn calibration_stale(&self) -> bool {
        match &self.engine {
            Engine::Fake(r) => r.engine.calibration_stale(),
            Engine::Live(e) => e.calibration_stale(),
        }
    }

    /// The inline session (fake mode only), for inspection.
    pub fn session(&self) -> Option<&Session> {
        match &self.driver {
            Driver::Inline(s) => Some(s),
            Driver::Thread(_) => None,
        }
    }

    pub fn policy(&self) -> &ReopenPolicy {
        &self.policy
    }

    fn host_now_ns(&self) -> Option<u64> {
        match &self.engine {
            Engine::Fake(r) => r.engine.host_now_ns(),
            Engine::Live(e) => e.host_now_ns(),
        }
    }

    fn clear_calibration_stale(&self) {
        match &self.engine {
            Engine::Fake(r) => r.engine.clear_calibration_stale(),
            Engine::Live(e) => e.clear_calibration_stale(),
        }
    }

    fn reopen(&mut self) -> Result<(MixerHandle, TapHandle), AudioError> {
        match &mut self.engine {
            Engine::Fake(r) => r.engine.reopen(),
            Engine::Live(e) => e.reopen(),
        }
    }

    // -------------------------------------------------------------- commands and link

    fn command(&mut self, c: AudioCommand) {
        let cmd = match c {
            AudioCommand::Soundcheck { force } => {
                self.cal_scheduled = false;
                SessionCmd::Soundcheck { force }
            }
            AudioCommand::StartRound {
                chart,
                lead_in_beats,
                take,
            } => {
                self.round_chart = Some(chart.clone());
                self.anchor = None;
                self.round_active = false;
                SessionCmd::StartRound {
                    chart,
                    lead_in_beats,
                    take,
                }
            }
            AudioCommand::StopRound => SessionCmd::StopRound,
            AudioCommand::Dispute => SessionCmd::Dispute,
            AudioCommand::SetLevel(l) => SessionCmd::SetLevel(l),
            AudioCommand::Record(on) => SessionCmd::Record(on),
            AudioCommand::CancelSoundcheck => {
                self.cal_scheduled = false;
                SessionCmd::CancelSoundcheck
            }
            AudioCommand::Retry => {
                self.policy.retry();
                return;
            }
        };
        self.port.send(cmd);
    }

    /// Device-loss bookkeeping: announces the loss, runs the reopen policy.
    fn upkeep_link(
        &mut self,
        dt_s: f64,
        status: &mut AudioStatus,
        links: &mut MessageWriter<AudioLink>,
    ) {
        if std::mem::take(&mut self.lost) {
            links.write(AudioLink::Lost);
            status.link = Link::Reopening { attempt: 1 };
        }
        let Some(attempt) = self.policy.tick(dt_s) else {
            return;
        };
        links.write(AudioLink::Retrying { attempt });
        status.link = Link::Reopening { attempt };
        match self.reopen() {
            Ok((mix, tap)) => {
                let info = self.info();
                self.play = PlayClock::new(f64::from(info.fs_out));
                self.port.send(SessionCmd::Rewire { mix, tap, info });
                self.policy.ok();
                self.anchor = None;
                self.cal_scheduled = false;
                status.link = Link::Up;
                links.write(AudioLink::Back {
                    epoch: self.epoch(),
                });
            }
            Err(e) => {
                if self.policy.failed() {
                    status.link = Link::Gone;
                    status.notice = Some(e.to_string());
                    links.write(AudioLink::Gone);
                }
            }
        }
    }

    /// Reads "the output frame sounding now" into the status; holds while the streams are
    /// closed.
    fn position(&mut self, status: &mut AudioStatus) {
        let Some(host) = self.host_now_ns() else {
            return;
        };
        if let Engine::Fake(r) = &self.engine
            && let Some(map) = r.engine.clock_map()
        {
            self.play.on_snapshot(map.snapshot());
        }
        let pos = self.play.frame_at(host, self.playhead.last_out());
        status.out_frame = pos.out_frame;
        status.play_mode = Some(pos.mode);
    }

    // -------------------------------------------------------------- driving

    /// Moves the audio side forward by one frame of `dt_s`; session events land in `self.out`.
    fn pump(&mut self, dt_s: f64) {
        let dt = dt_s.clamp(0.0, MAX_FRAME_S);
        if let Engine::Live(engine) = &mut self.engine {
            engine.poll();
            self.out.extend(self.port.drain());
            self.react_to_new_events(false);
            return;
        }
        self.carry_s += dt;
        while self.carry_s >= SLICE_S - 1.0e-12 {
            self.carry_s -= SLICE_S;
            if let Engine::Fake(rig) = &mut self.engine {
                rig.advance(SLICE_S);
                if let Driver::Inline(s) = &mut self.driver {
                    s.step();
                }
                self.out.extend(self.port.drain());
                Self::fake_player(
                    rig,
                    &self.driver,
                    self.player.as_ref(),
                    &mut self.cal_scheduled,
                );
            }
            self.react_to_new_events(true);
        }
    }

    /// Schedules the virtual player for the calibration's strum step, once per soundcheck.
    fn fake_player(
        rig: &mut FakeRig,
        driver: &Driver,
        player: Option<&PlayerModel>,
        scheduled: &mut bool,
    ) {
        let (Driver::Inline(s), Some(model)) = (driver, player) else {
            return;
        };
        match s.calibration_beat_plan() {
            Some(plan) if !*scheduled => {
                *scheduled = true;
                let fs = rig.truth().map_or(48_000.0, |t| t.fs_out);
                let hits: Vec<ChartHit> = (0..plan.beats)
                    .map(|i| ChartHit {
                        id: i,
                        t_s: (plan.strum_frame(i) - plan.start) as f64 / fs,
                        kind: HitKind::Mute,
                    })
                    .collect();
                rig.schedule_player(plan.start, &hits, model);
            }
            _ => {}
        }
    }

    /// Bookkeeping the bridge itself owes to some events. `new` events are the tail of
    /// `self.out` not yet seen.
    fn react_to_new_events(&mut self, fake: bool) {
        let epoch = self.epoch();
        let n = self.out.len();
        for i in self.seen..n {
            match self.out[i].clone() {
                SessionEvent::RoundArmed { anchor, .. } => {
                    self.round_active = true;
                    let first = self.anchor.is_none_or(|(a, _)| a != anchor);
                    self.anchor = Some((anchor, epoch));
                    if fake
                        && first
                        && let (Engine::Fake(rig), Some(model), Some(chart)) = (
                            &mut self.engine,
                            self.player.as_ref(),
                            self.round_chart.as_ref(),
                        )
                    {
                        rig.schedule_player(anchor, chart, model);
                    }
                }
                SessionEvent::RoundDone { .. } => self.round_active = false,
                SessionEvent::SoundcheckDone { basis, outcome } => {
                    self.basis = Some(basis);
                    self.cal_scheduled = false;
                    if matches!(outcome, SoundOutcome::Kept | SoundOutcome::Calibrated) {
                        self.clear_calibration_stale();
                    }
                }
                SessionEvent::Paused(_) => {
                    self.round_active = false;
                    self.lost = true;
                    self.policy.start();
                }
                _ => {}
            }
        }
        self.seen = n;
    }
}

impl Drop for AudioRig {
    fn drop(&mut self) {
        self.port.send(SessionCmd::Shutdown);
        if let Driver::Inline(s) = &mut self.driver {
            s.step();
        }
    }
}

fn host_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "CoreAudio"
    } else if cfg!(target_os = "windows") {
        "WASAPI"
    } else {
        "ALSA"
    }
}

/// Today's date, `YYYY-MM-DD` (UTC), for new calibration profiles.
fn civil_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn session_config(host: &str, opts: &BridgeOptions) -> SessionConfig {
    let mut cfg = SessionConfig::new(host, civil_date());
    cfg.video_offset_ms = opts.video_offset_ms;
    match &opts.data {
        Some(dir) => {
            cfg.profiles = Some(dir.join("profiles.json"));
            if opts.tape != Some(false) {
                cfg.tapes = Some(dir.join("tapes"));
            }
        }
        None if host != "fake" => cfg.profiles = default_profile_path(),
        None => {}
    }
    cfg.record = cfg.tapes.is_some() && opts.tape.unwrap_or(TAPES_DEFAULT_ON);
    cfg
}

// ------------------------------------------------------------------ the plugin

/// Adds the bridge: messages, resources, the `PreUpdate` system in [`AudioSet`].
pub struct AudioBridgePlugin {
    pub source: Source,
    pub opts: BridgeOptions,
    /// The fake devices (only read for [`Source::Fake`]).
    pub fake: FakeConfig,
}

impl AudioBridgePlugin {
    pub fn fake(fake: FakeConfig, opts: BridgeOptions) -> Self {
        Self {
            source: Source::Fake,
            opts,
            fake,
        }
    }

    pub fn live(opts: BridgeOptions) -> Self {
        Self {
            source: Source::Live,
            opts,
            fake: bench_room(),
        }
    }
}

/// The fake room of `--fake`: 87.3 ms of acoustic path and a little noise.
pub fn bench_room() -> FakeConfig {
    let mut fc = FakeConfig::new(48_000, 48_000);
    fc.path.latency_s = 0.0873;
    fc.path.noise_rms = 1.0e-3;
    fc
}

impl Plugin for AudioBridgePlugin {
    fn build(&self, app: &mut App) {
        register(app);
        app.insert_resource(VideoOffset(self.opts.video_offset_ms / 1.0e3));
        let made = match self.source {
            Source::Fake => AudioRig::fake(self.fake.clone(), &self.opts),
            Source::Live => AudioRig::live(&self.opts),
        };
        let mut status = AudioStatus::default();
        match made {
            Ok(rig) => {
                status.link = Link::Up;
                status.fs_out = rig.info().fs_out;
                app.insert_resource(rig.mode());
                app.insert_non_send(rig);
            }
            Err(e) => {
                status.link = Link::Unavailable;
                status.notice = Some(e.to_string());
                eprintln!("audio: {e}");
            }
        }
        app.insert_resource(status);
    }
}

/// Messages, resources and the system, shared by every mode (so G2-G4 can always read and
/// write the messages, also when the mode is `None`).
fn register(app: &mut App) {
    app.add_message::<AudioCommand>()
        .add_message::<AudioNote>()
        .add_message::<AudioLink>()
        .init_resource::<AudioStatus>()
        .init_resource::<VideoOffset>()
        .configure_sets(PreUpdate, AudioSet)
        .add_systems(PreUpdate, drive.in_set(AudioSet));
}

/// Called once by `build_app` after the base plugins and resources are in place.
pub fn install(app: &mut App, cfg: &AppConfig) {
    let parsed = parse_args(&cfg.args);
    for w in &parsed.warnings {
        eprintln!("audio: {w}");
    }
    // Screenshots run on the frame clock.
    let source = if cfg.shot_dir.is_some() {
        None
    } else {
        parsed.source
    };
    match source {
        None => {
            register(app);
            app.insert_resource(AudioMode::None);
        }
        Some(Source::Fake) => {
            app.add_plugins(AudioBridgePlugin::fake(bench_room(), parsed.opts));
        }
        Some(Source::Live) => {
            app.add_plugins(AudioBridgePlugin::live(parsed.opts));
        }
    }
}

// ------------------------------------------------------------------ the system

#[allow(clippy::too_many_arguments)]
fn drive(
    time: Res<Time<Real>>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    video: Res<VideoOffset>,
    rig: Option<NonSendMut<AudioRig>>,
    mut cmds: MessageReader<AudioCommand>,
    mut notes: MessageWriter<AudioNote>,
    mut links: MessageWriter<AudioLink>,
    mut status: ResMut<AudioStatus>,
    mut clock: ResMut<SongClock>,
) {
    let Some(mut rig) = rig else {
        // No rig: nothing to drive. Commands are dropped; the link says why.
        cmds.clear();
        return;
    };
    let dt = time.delta_secs_f64();
    let rig = &mut *rig;

    // Commands from the game, then the player's key for a lost device.
    for c in cmds.read() {
        rig.command(c.clone());
    }
    if status.link == Link::Gone && keys.is_some_and(|k| k.just_pressed(KeyCode::KeyR)) {
        rig.policy.retry();
    }

    // The audio side, then the link policy (frame time, not wall time: deterministic).
    rig.pump(dt);
    rig.upkeep_link(dt, &mut status, &mut links);
    for e in rig.out.drain(..) {
        notes.write(AudioNote(e));
    }
    rig.seen = 0;

    // Status and song clock.
    let epoch = rig.epoch();
    if rig.anchor.is_some_and(|(_, ae)| ae != epoch) {
        // The streams were reopened: the old anchor means nothing.
        rig.anchor = None;
    }
    status.epoch = epoch;
    status.fs_out = rig.info().fs_out;
    status.calibration_stale = rig.calibration_stale();
    status.anchor = rig.anchor.map(|(a, _)| a);
    status.round_active = rig.round_active;
    status.basis = rig.basis;
    rig.position(&mut status);
    if clock.running {
        match (rig.anchor, status.play_mode) {
            (Some((anchor, _)), Some(mode)) if mode != PlayMode::Hold => {
                let fs = f64::from(status.fs_out);
                clock.t = ((status.out_frame - anchor as f64) / fs - video.0) as f32;
            }
            (None, _) => clock.t = LEAD_IN,
            _ => {}
        }
    }
}
