//! The session loop (owned by S2).
//!
//! One [`Session`] owns the engine's client side ([`EngineIo`]) and runs, on its own thread
//! (or stepped by hand in a test), the whole game round trip:
//!
//! ```text
//! EngineEvent --> tape --> Adapter --> PulseJudge --> VerdictLog --> SessionEvent
//!                                          ^                            (to the UI)
//! SessionCmd (from the UI) ----------------+--> mixer (click) / calibrator / tape
//! ```
//!
//! # Rules the loop keeps
//!
//! - **I2**: the judge's inputs are a function of the engine's events and the chart, nothing
//!   else; every engine event and every command goes on the tape in the order it was handled,
//!   and an `Anchor` line means "a new judge starts here".
//! - **I3**: a round is judged with the timing reference frozen at its start, and only a
//!   calibration that was verified this run (`Soundcheck`) and holds on the current clock
//!   map ([`Profile::holds_on`]) counts; otherwise timing is not judged (`NoCalibration`) and
//!   a [`Notice::SoundcheckAdvised`] says why.
//! - **I5**: a calibration never runs while a round does. A `Soundcheck` sent during a round
//!   waits for its end; a `StartRound` sent during a soundcheck waits for its end.
//! - Tapes are written by the `guitarness-tape` thread, never by the audio callbacks and never
//!   by the judge path; they are off unless the session was configured or told to record
//!   ([`TAPES_DEFAULT_ON`]).
//!
//! # Time
//!
//! A round starts `0.5 s` (plus `0.25 s` per retry) after the newest output frame the clock
//! knows. The count-in is a metronome of `lead_in_beats` clicks; the chart's own clicks
//! follow in one metronome per tempo segment, sent a few seconds ahead of time. The click
//! of event `k` sounds at `anchor + round(t_k * fs_out)` (within one frame: the mixer rounds
//! `start + round(n * period)`), and the judge is told its input frame
//! (`in_of_out`) on the newest clock map, again after a clock step.
//!
//! # Epochs and rewires
//!
//! Every engine event that carries an epoch older than the newest one seen (or than the
//! epoch of the handles a `Rewire` brought) is stale: it is taped and changes nothing. A
//! `Stopped` carries no epoch, but the old streams' `Stopped` always comes before the
//! `Reopened` of the new ones: while a `Rewire` is ahead of its `Reopened` (counted, and
//! healed by any event of a newer epoch should the `Reopened` be lost) a `Stopped` is old
//! news. The contract: send one `Rewire` after every successful
//! [`guitarness_audio::AudioEngine::reopen`].
//!
//! # Tapes
//!
//! Every `Anchor` line carries the timing reference and judge settings of its round; the
//! header holds those of the latest round. `Record(false)` during a round closes the tape
//! when the round ends (closing syncs files, which the judge thread must not wait for in
//! the middle of a round). The raw ring is handed back when a tape closes, so every tape of
//! an epoch has audio.
//!
//! # Follow-up
//!
//! The round loop here and the one in `replay` are two copies of the same rules; a shared
//! round runner is planned.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use guitarness_audio::calib::Basis;
use guitarness_audio::{
    BackendEvent, CalibConfig, Calibrator, ClockSnapshot, EngineEvent, EngineIo, MixCmd, MixEvent,
    MixerHandle, Outcome, Profile, ProfileKey, Prompt, Side, StreamInfo, TapHandle, find_profile,
    profiles_from_json, profiles_to_json, upsert_profile,
};
use guitarness_core::chart::{Chart, Loc};
use guitarness_core::contract::{EventContract, contract};
use guitarness_core::gates::GateTable;
use guitarness_core::judge::{JudgeConfig, PulseJudge};
use guitarness_core::pulse;
use guitarness_core::stats::{Improvement, RoundStats, VerdictLog, tolerance_suggestion};
use guitarness_core::timing::{Level, TimingRef};
use guitarness_core::verdict::{Judge, JudgeInput, JudgeOut};

use crate::adapter::Adapter;
use crate::format::{SessionCmdLog, TapeEvent, TapeHeader, new_tape_dir};
use crate::rig::TruthStrum;
use crate::writer::{KeptRaw, TapeSummary, TapeWriter};

/// Whether sessions record tapes unless told otherwise. Tapes are opt in.
pub const TAPES_DEFAULT_ON: bool = false;
/// Mixer tag of the clicks of a round: `ROUND_TAG + attempt`. Calibrator tags are far above.
pub const ROUND_TAG: u32 = 0x5055_4C00;
/// Mixer tag of a click track with no round ([`SessionCmd::Click`]).
pub const CLICK_TAG: u32 = ROUND_TAG + 0x80;
/// Times a round is armed again when the mixer says its first click came late.
pub const MAX_ATTEMPTS: u32 = 3;
/// A click segment is sent to the mixer when it starts within this many output seconds.
pub const SEGMENT_AHEAD_S: f64 = 3.0;
/// Time from "now" to the first click of a round, output seconds (plus `0.25 s` per retry).
pub const START_DELAY_S: f64 = 0.5;

/// What a session is made with.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Host API name, part of the profile key (`"fake"` for the fake backend).
    pub host: String,
    /// Date stamped on a new profile, `YYYY-MM-DD`.
    pub date: String,
    /// Calibration profiles file. `None`: nothing is read or saved.
    pub profiles: Option<PathBuf>,
    /// Folder that holds the tapes. `None`: no tape is ever written.
    pub tapes: Option<PathBuf>,
    /// Record a tape from the start (needs `tapes`). Default [`TAPES_DEFAULT_ON`].
    pub record: bool,
    /// Tolerance level of the first round.
    pub level: Level,
    /// The gate table the judge and the statistics follow.
    pub gates: GateTable,
    /// Gain of the clicks, 0 to 1.
    pub click_gain: f32,
    /// Added to the judge's `close_margin_ms` for a host that delivers late, ms.
    pub close_margin_extra_ms: f64,
    /// Offset between a screen recording and the audio, written to the tape header, ms.
    pub video_offset_ms: f64,
}

impl SessionConfig {
    /// Defaults: no files, no tapes, apprentice level, built-in gates.
    pub fn new(host: impl Into<String>, date: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            date: date.into(),
            profiles: None,
            tapes: None,
            record: TAPES_DEFAULT_ON,
            level: Level::Apprentice,
            gates: GateTable::builtin(),
            click_gain: 0.7,
            close_margin_extra_ms: 0.0,
            video_offset_ms: 0.0,
        }
    }
}

/// `$HOME/.guitarness/profiles.json` (`USERPROFILE` on Windows), the file `guitarness calibrate`
/// writes.
pub fn default_profile_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".guitarness")
            .join("profiles.json"),
    )
}

/// The timing reference a round freezes from a profile on this clock map. `None` for the
/// profile or the snapshot, a profile that does not hold on the snapshot's stamp sources, or
/// one with no basis, is [`TimingRef::none`]. What the judge subtracts is exactly
/// [`Profile::judge_offset_ms`]: the loop on a loopback, the strums' combined number on an
/// estimate. The player's tendency is never part of it.
pub fn timing_ref(profile: Option<&Profile>, snap: Option<&ClockSnapshot>) -> TimingRef {
    let (Some(p), Some(s)) = (profile, snap) else {
        return TimingRef::none();
    };
    if !p.holds_on(s) {
        return TimingRef::none();
    }
    match p.basis() {
        Basis::Loopback => match (p.l_rt_ms, p.mad_ms) {
            (Some(l), Some(mad)) => TimingRef::loopback(l, mad),
            _ => TimingRef::none(),
        },
        Basis::Estimate => match (p.combined_ms, p.combined_mad_ms, p.combined_n) {
            (Some(off), Some(mad), Some(n)) => TimingRef::estimate(off, mad, n),
            _ => TimingRef::none(),
        },
        Basis::None => TimingRef::none(),
    }
}

/// What the UI asks of the session.
#[allow(clippy::large_enum_variant)]
pub enum SessionCmd {
    /// Check (or measure) the audio setup. `force`: measure again even if a verified
    /// profile holds. Deferred while a round runs.
    Soundcheck { force: bool },
    /// Play `chart` against the click, after `lead_in_beats` clicks of count-in.
    StartRound {
        chart: Chart,
        lead_in_beats: u32,
        take: Option<String>,
    },
    /// End the running round now; its statistics are of what was judged so far.
    StopRound,
    /// The player contests the last final verdict shown.
    Dispute,
    /// Tolerance level from the next round on.
    SetLevel(Level),
    /// Start or stop recording tapes (needs `SessionConfig::tapes`).
    Record(bool),
    /// The audio was reopened ([`guitarness_audio::AudioEngine::reopen`]): new mixer and tap
    /// handles and the new stream info.
    Rewire {
        mix: MixerHandle,
        tap: TapHandle,
        info: StreamInfo,
    },
    /// Stop everything and close the tape.
    Shutdown,
    /// A take starts here: the label goes on the tape (no round needed).
    Take(String),
    /// A click track with no judge: `beats` clicks at `bpm`, an accent every `accent_every`
    /// beats (0: none), starting [`START_DELAY_S`] from now. `StopRound`, a new round and a
    /// soundcheck stop it. Refused during a round; waits for a soundcheck.
    Click {
        bpm: f64,
        beats: u32,
        accent_every: u8,
    },
    /// Stop the soundcheck that runs (or waits): no profile is written, the verification
    /// in force stays, [`SessionEvent::SoundcheckCancelled`] says so, and a round that
    /// waited for the soundcheck starts at once.
    CancelSoundcheck,
}

/// What the soundcheck ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundOutcome {
    /// The saved profile still holds.
    Kept,
    /// A new profile was measured and saved.
    Calibrated,
    /// The clock never settled: nothing was measured.
    Unstable,
    /// The microphone delivers silence.
    MicBlocked,
    /// A device event, a rewire or a shutdown ended it.
    Interrupted,
}

/// Why a soundcheck is advised before a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advice {
    /// No profile for this device pair.
    NoProfile,
    /// A profile exists but was not verified in this run.
    NotChecked,
    /// The profile was made on other clock stamps: it does not apply to this map.
    NotHolding,
    /// The device changed or restarted since the last check.
    DeviceChanged,
    /// The profile has no basis to judge timing with.
    NoBasis,
}

/// What a command had to wait for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deferred {
    /// A soundcheck waits for the round to end.
    Soundcheck,
    /// A round waits for the soundcheck (or the first clock snapshot).
    Round,
}

/// Why a command was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A round is already running.
    RoundRunning,
    /// The device stopped and was not rewired yet.
    Paused,
    /// The chart is invalid or has nothing to play.
    BadChart,
    /// The mixer had no room for a click track.
    ClickRejected,
}

/// Something the UI may want to say that is not a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A round starts without timing: a soundcheck would fix it.
    SoundcheckAdvised(Advice),
    /// A command waits.
    Deferred(Deferred),
    /// A command was not done.
    Refused(Refusal),
    /// The profiles file could not be written.
    ProfilesNotSaved,
}

/// Why the session waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseWhy {
    /// The device stopped; send `Rewire` after reopening.
    DeviceStopped(BackendEvent),
}

/// What happened to the tape.
#[derive(Clone, Debug, PartialEq)]
pub enum TapeNote {
    /// A tape was opened.
    Started { dir: PathBuf, audio: bool },
    /// A tape was closed cleanly (see `complete` in the summary).
    Closed(TapeSummary),
    /// The tape stopped: an I/O error, or the event queue overflowed. The session goes on.
    Failed(String),
}

/// What the session tells the UI.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum SessionEvent {
    /// What the calibration asks of the player.
    Prompt(Prompt),
    /// The soundcheck ended. `basis` is what the profile in force judges with.
    SoundcheckDone { basis: Basis, outcome: SoundOutcome },
    /// The soundcheck was cancelled (`CancelSoundcheck`): nothing was measured or saved.
    /// `basis` is what the next round judges with, as before the soundcheck.
    SoundcheckCancelled { basis: Basis },
    /// A round is armed: chart time zero is output frame `anchor` (sent again, with the new
    /// anchor, if the first clicks came late and the round was armed again).
    RoundArmed { anchor: u64, fs_out: u32 },
    /// What the judge said.
    Judge(JudgeOut),
    /// A round ended. `interrupted`: stopped, or the device went away.
    RoundDone {
        stats: RoundStats,
        better: Vec<Improvement>,
        suggest: Option<Level>,
        interrupted: bool,
    },
    /// The statistics of the last round after a dispute.
    RoundRevised { stats: RoundStats },
    /// The session waits for a `Rewire`.
    Paused(PauseWhy),
    /// A tape opened, closed or failed.
    Tape(TapeNote),
    /// Something to say.
    Notice(Notice),
}

/// The UI's end: commands in, events out.
pub struct SessionPort {
    tx: Sender<SessionCmd>,
    rx: Mutex<Receiver<SessionEvent>>,
}

impl SessionPort {
    /// Sends a command. `false` once the session is gone.
    pub fn send(&self, c: SessionCmd) -> bool {
        self.tx.send(c).is_ok()
    }

    /// The next event, if there is one.
    pub fn try_recv(&self) -> Option<SessionEvent> {
        self.rx.lock().ok()?.try_recv().ok()
    }

    /// Every event waiting.
    pub fn drain(&self) -> Vec<SessionEvent> {
        let mut v = Vec::new();
        while let Some(e) = self.try_recv() {
            v.push(e);
        }
        v
    }
}

/// A session running on its own thread.
pub struct SessionThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Session>>,
}

impl SessionThread {
    /// Stops the loop (the tape is closed) and gives the session back; `None` if the
    /// thread panicked.
    pub fn stop(mut self) -> Option<Session> {
        self.stop.store(true, Ordering::Release);
        self.handle.take()?.join().ok()
    }
}

impl Drop for SessionThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ------------------------------------------------------------------ internals

/// One metronome of a round: a run of quarter notes at one tempo.
#[derive(Clone, Copy, Debug)]
struct Seg {
    start: u64,
    period: f64,
    accent: u8,
    beats: u32,
}

/// The click runs of `chart`, one per tempo segment, for chart time zero at `anchor`.
fn click_segments(chart: &Chart, anchor: u64, fs_out: f64) -> Vec<Seg> {
    let tpq = f64::from(chart.tpq.max(1));
    let end = chart
        .events()
        .iter()
        .map(|e| e.tick + e.dur)
        .max()
        .unwrap_or(0);
    let mut pts: Vec<(u32, f64)> = chart.tempo_map.iter().map(|p| (p.tick, p.bpm)).collect();
    if pts.first().is_none_or(|p| p.0 > 0) {
        pts.insert(0, (0, chart.bpm_at(0)));
    }
    pts.sort_by_key(|p| p.0);
    pts.dedup_by_key(|p| p.0);
    let mut out = Vec::new();
    for (i, &(tick, bpm)) in pts.iter().enumerate() {
        if tick >= end {
            break;
        }
        let next = pts.get(i + 1).map_or(end, |p| p.0.min(end));
        let beats = (f64::from(next - tick) / tpq).ceil() as u32;
        if beats == 0 || !(bpm.is_finite() && bpm > 0.0) {
            continue;
        }
        out.push(Seg {
            start: anchor + (chart.seconds_of_tick(tick) * fs_out).round() as u64,
            period: 60.0 / bpm * fs_out,
            accent: chart.sig_at(tick).num,
            beats,
        });
    }
    out
}

/// Epoch of the engine events that carry one.
fn event_epoch(e: &EngineEvent) -> Option<u32> {
    match e {
        EngineEvent::Clock(s) => Some(s.epoch()),
        EngineEvent::Onset(n) => Some(n.epoch),
        EngineEvent::Unjudged { epoch, .. } | EngineEvent::Reopened { epoch } => Some(*epoch),
        EngineEvent::Step(s) => Some(s.epoch),
        _ => None,
    }
}

/// A round: armed, judged, finished (and kept until the next one, for a dispute).
struct Round {
    chart: Chart,
    lead_in: u32,
    n: usize,
    judge: PulseJudge,
    jcfg: JudgeConfig,
    log: VerdictLog,
    timing: TimingRef,
    level: Level,
    tag: u32,
    attempt: u32,
    anchor: u64,
    segs: Vec<Seg>,
    next_seg: usize,
    beat_seen: bool,
}

/// A soundcheck: the calibrator and what it was asked.
struct Check {
    cal: Calibrator,
}

struct PendingRound {
    chart: Chart,
    lead_in: u32,
    take: Option<String>,
}

/// The session. See the module documentation.
pub struct Session {
    io: EngineIo,
    info: StreamInfo,
    cfg: SessionConfig,
    key: ProfileKey,
    cmd_rx: Receiver<SessionCmd>,
    ev_tx: Sender<SessionEvent>,
    adapter: Adapter,
    profiles: Vec<Profile>,
    verified: bool,
    device_changed: bool,
    level: Level,
    record: bool,
    snap: Option<ClockSnapshot>,
    paused: bool,
    held: Vec<EngineEvent>,
    /// `Reopened` events seen minus `Rewire`s handled: below zero, a `Rewire` is ahead of
    /// the `Reopened` of its streams, and a `Stopped` is the old streams'.
    reopen_lag: i64,
    /// Newest epoch seen in a handled event.
    max_epoch: u32,
    /// `max_epoch` when the last `Rewire` came.
    rewire_epoch: u32,
    /// Close the tape when the running round ends (`Record(false)` mid-round).
    close_after_round: bool,
    /// The raw ring of the tap, kept between two tapes of one epoch.
    raw_spare: Option<KeptRaw>,
    /// A click track (`Click`) was sent to the mixer and not stopped.
    click_on: bool,
    pending_click: Option<(f64, u32, u8)>,
    round: Option<Round>,
    finished: Option<Round>,
    prev_stats: Option<RoundStats>,
    check: Option<Check>,
    pending_round: Option<PendingRound>,
    pending_soundcheck: Option<bool>,
    outbox: VecDeque<MixCmd>,
    tape: Option<TapeWriter>,
    tape_noted: bool,
    ins: Vec<JudgeInput>,
    outs: Vec<JudgeOut>,
    done: bool,
}

impl Session {
    /// A session on the client side of an engine. Reads the profiles file if there is one;
    /// opens a tape if the configuration says to record.
    pub fn new(io: EngineIo, info: StreamInfo, cfg: SessionConfig) -> (Session, SessionPort) {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let profiles = cfg
            .profiles
            .as_deref()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|t| profiles_from_json(&t))
            .unwrap_or_default();
        let key = ProfileKey::new(cfg.host.clone(), &info);
        let adapter = Adapter::new(f64::from(info.fs_in), f64::from(info.fs_out));
        let mut s = Session {
            io,
            level: cfg.level,
            record: cfg.record,
            info,
            key,
            cmd_rx,
            ev_tx,
            adapter,
            profiles,
            verified: false,
            device_changed: false,
            snap: None,
            paused: false,
            held: Vec::new(),
            reopen_lag: 0,
            max_epoch: 0,
            rewire_epoch: 0,
            close_after_round: false,
            raw_spare: None,
            click_on: false,
            pending_click: None,
            round: None,
            finished: None,
            prev_stats: None,
            check: None,
            pending_round: None,
            pending_soundcheck: None,
            outbox: VecDeque::new(),
            tape: None,
            tape_noted: false,
            ins: Vec::new(),
            outs: Vec::new(),
            done: false,
            cfg,
        };
        if s.record {
            s.open_tape();
        }
        let port = SessionPort {
            tx: cmd_tx,
            rx: Mutex::new(ev_rx),
        };
        (s, port)
    }

    /// Runs the loop on its own thread, one [`Session::step`] every `period`.
    pub fn spawn(mut self, period: Duration) -> SessionThread {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = thread::Builder::new()
            .name("guitarness-session".into())
            .spawn(move || {
                while !flag.load(Ordering::Acquire) && !self.done {
                    self.step();
                    thread::sleep(period);
                }
                self.close_tape();
                self
            })
            .expect("the session thread starts");
        SessionThread {
            stop,
            handle: Some(handle),
        }
    }

    /// True after `Shutdown`.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// True while a round is armed and being judged.
    pub fn round_active(&self) -> bool {
        self.round.is_some()
    }

    /// True while a soundcheck runs.
    pub fn soundcheck_active(&self) -> bool {
        self.check.is_some()
    }

    /// True after a soundcheck verified the profile in this run and nothing changed since.
    pub fn verified(&self) -> bool {
        self.verified
    }

    /// The profile of the device pair now open, if any.
    pub fn profile(&self) -> Option<&Profile> {
        find_profile(&self.profiles, &self.key)
    }

    /// The tape folder, while one is open.
    pub fn tape_dir(&self) -> Option<&Path> {
        self.tape.as_ref().map(TapeWriter::dir)
    }

    /// The click schedule of the calibration's strum step, once it is known (for a virtual
    /// player on the fake rig, and for the UI's beat display).
    pub fn calibration_beat_plan(&self) -> Option<guitarness_audio::BeatPlan> {
        self.check.as_ref().and_then(|c| c.cal.beat_plan())
    }

    /// Writes what the virtual player really did on the tape (test bench only).
    pub fn tape_truth(&self, truth: &[TruthStrum]) {
        if let Some(t) = &self.tape {
            for s in truth {
                t.event(TapeEvent::Truth(s.clone()));
            }
        }
    }

    /// The timing reference a round started now would freeze.
    pub fn next_timing(&self) -> TimingRef {
        self.timing_and_advice().0
    }

    /// What the event `loc` of `chart` asks of the player and which of it the microphone
    /// verifies in a round started now: the K2 contract, from the same gate table and the
    /// same timing reference the judge gets.
    pub fn contract(&self, chart: &Chart, loc: Loc) -> EventContract {
        let judged = self.next_timing().judges_timing();
        contract(chart, loc, &self.cfg.gates, judged)
    }

    // ------------------------------------------------------------ the loop

    /// One turn: engine events, the calibrator, commands, then the upkeep of the round.
    /// Call it every 5 to 20 ms.
    pub fn step(&mut self) {
        if self.done {
            return;
        }
        let mut live: Vec<EngineEvent> = Vec::new();
        while let Ok(e) = self.io.events.pop() {
            if self.paused {
                self.held.push(e);
            } else {
                self.handle_event(&e);
                live.push(e);
            }
        }
        self.step_check(&live);
        while let Ok(c) = self.cmd_rx.try_recv() {
            self.handle_cmd(c);
        }
        self.upkeep();
    }

    fn emit(&self, e: SessionEvent) {
        let _ = self.ev_tx.send(e);
    }

    fn notice(&self, n: Notice) {
        self.emit(SessionEvent::Notice(n));
    }

    fn log_cmd(&self, c: SessionCmdLog) {
        if let Some(t) = &self.tape {
            t.event(TapeEvent::Cmd(c));
        }
    }

    // ------------------------------------------------------------ engine events

    fn handle_event(&mut self, e: &EngineEvent) {
        if let Some(t) = &self.tape
            && !matches!(e, EngineEvent::Pitch(_))
        {
            t.event(TapeEvent::Engine(*e));
        }
        let epoch = event_epoch(e);
        let stale = epoch.is_some_and(|ep| self.adapter.is_stale(ep));
        if let Some(ep) = epoch
            && !stale
        {
            if self.reopen_lag < 0
                && ep > self.rewire_epoch
                && !matches!(e, EngineEvent::Reopened { .. })
            {
                // The new streams speak, and their `Reopened` never came: it was lost.
                self.reopen_lag = 0;
            }
            self.max_epoch = self.max_epoch.max(ep);
        }
        match e {
            EngineEvent::Clock(s) if !stale => self.snap = Some(*s),
            EngineEvent::Mix(m) => self.on_mix(m),
            EngineEvent::Backend(
                BackendEvent::DeviceChanged | BackendEvent::DeviceLost | BackendEvent::Invalidated,
            ) => self.unverify(),
            EngineEvent::Stopped(why) => {
                if self.reopen_lag < 0 {
                    // The streams a `Rewire` already replaced: that stop is old news, and
                    // neither the judge nor the round of the new streams hears of it.
                    return;
                }
                self.unverify();
                self.snap = None;
                self.paused = true;
                self.emit(SessionEvent::Paused(PauseWhy::DeviceStopped(*why)));
            }
            EngineEvent::Reopened { .. } => {
                self.reopen_lag += 1;
                self.snap = None;
                self.unverify();
            }
            _ => {}
        }
        self.ins.clear();
        let mut ins = std::mem::take(&mut self.ins);
        self.adapter.on_engine(e, &mut ins);
        self.drive(&ins);
        self.ins = ins;
        if matches!(e, EngineEvent::Stopped(_) | EngineEvent::Reopened { .. })
            && self.round.is_some()
        {
            self.finish_round(true);
        }
    }

    fn unverify(&mut self) {
        self.verified = false;
        self.device_changed = true;
    }

    fn on_mix(&mut self, m: &MixEvent) {
        let Some(r) = self.round.as_mut() else {
            return;
        };
        match *m {
            MixEvent::Beat { tag, .. } if tag == r.tag => r.beat_seen = true,
            MixEvent::Late { tag, .. } if tag == r.tag && !r.beat_seen => {
                if r.attempt + 1 < MAX_ATTEMPTS {
                    let next = r.attempt + 1;
                    self.arm(next);
                }
            }
            MixEvent::Rejected { tag } if tag == r.tag => {
                self.notice(Notice::Refused(Refusal::ClickRejected));
            }
            _ => {}
        }
    }

    /// Feeds the judge of the running round and handles what it says.
    fn drive(&mut self, ins: &[JudgeInput]) {
        let Some(r) = self.round.as_mut() else {
            return;
        };
        self.outs.clear();
        for i in ins {
            r.judge.push(i, &mut self.outs);
        }
        for o in &self.outs {
            r.log.push(o);
            if let Some(t) = &self.tape {
                t.event(TapeEvent::Out(*o));
            }
            let _ = self.ev_tx.send(SessionEvent::Judge(*o));
        }
    }

    // ------------------------------------------------------------ commands

    fn handle_cmd(&mut self, c: SessionCmd) {
        match c {
            SessionCmd::Soundcheck { force } => {
                self.log_cmd(SessionCmdLog::Soundcheck { force });
                self.cmd_soundcheck(force);
            }
            SessionCmd::StartRound {
                chart,
                lead_in_beats,
                take,
            } => {
                self.log_cmd(SessionCmdLog::StartRound {
                    lead_in_beats,
                    take: take.clone(),
                });
                self.cmd_start_round(PendingRound {
                    chart,
                    lead_in: lead_in_beats,
                    take,
                });
            }
            SessionCmd::StopRound => {
                self.log_cmd(SessionCmdLog::StopRound);
                self.pending_round = None;
                self.pending_click = None;
                self.stop_click();
                if self.round.is_some() {
                    self.finish_round(true);
                }
            }
            SessionCmd::Dispute => {
                self.log_cmd(SessionCmdLog::Dispute);
                self.cmd_dispute();
            }
            SessionCmd::SetLevel(l) => {
                self.log_cmd(SessionCmdLog::SetLevel(l));
                self.level = l;
            }
            SessionCmd::Record(on) => {
                if on {
                    self.record = true;
                    self.close_after_round = false;
                    if self.tape.is_none() {
                        self.open_tape();
                    }
                    self.log_cmd(SessionCmdLog::Record(true));
                } else {
                    self.log_cmd(SessionCmdLog::Record(false));
                    self.record = false;
                    if self.round.is_some() {
                        self.close_after_round = true;
                    } else {
                        self.close_tape();
                    }
                }
            }
            SessionCmd::Rewire { mix, tap, info } => {
                self.log_cmd(SessionCmdLog::Rewire);
                self.cmd_rewire(mix, tap, info);
            }
            SessionCmd::Take(label) => {
                // The `take` line is the command's own record.
                if let Some(t) = &self.tape {
                    t.event(TapeEvent::Take { label });
                }
            }
            SessionCmd::Click {
                bpm,
                beats,
                accent_every,
            } => {
                self.log_cmd(SessionCmdLog::Click {
                    bpm,
                    beats,
                    accent_every,
                });
                self.cmd_click(bpm, beats, accent_every);
            }
            SessionCmd::CancelSoundcheck => {
                self.log_cmd(SessionCmdLog::CancelSoundcheck);
                self.cmd_cancel_soundcheck();
            }
            SessionCmd::Shutdown => {
                self.log_cmd(SessionCmdLog::Shutdown);
                self.pending_round = None;
                self.pending_soundcheck = None;
                if let Some(mut c) = self.check.take() {
                    c.cal.cancel(&mut self.io.mix, &mut self.io.tap);
                    self.finish_check_outcome(&c.cal);
                }
                if self.round.is_some() {
                    self.finish_round(true);
                }
                self.flush_outbox();
                self.close_tape();
                self.done = true;
            }
        }
    }

    /// The tape is marked disputed by the writer when a `Withdrawn { why: Disputed }` line
    /// comes, so a dispute with nothing to take back leaves it unmarked.
    fn cmd_dispute(&mut self) {
        if self.round.is_some() {
            self.drive(&[JudgeInput::Dispute]);
            return;
        }
        let Some(r) = self.finished.as_mut() else {
            return;
        };
        self.outs.clear();
        r.judge.push(&JudgeInput::Dispute, &mut self.outs);
        let changed = !self.outs.is_empty();
        for o in &self.outs {
            r.log.push(o);
            if let Some(t) = &self.tape {
                t.event(TapeEvent::Out(*o));
            }
            let _ = self.ev_tx.send(SessionEvent::Judge(*o));
        }
        if changed {
            let stats = r.log.stats(&r.timing).gated(&self.cfg.gates);
            if let Some(t) = &self.tape {
                t.event(TapeEvent::Round(stats));
            }
            self.emit(SessionEvent::RoundRevised { stats });
        }
    }

    fn cmd_rewire(&mut self, mix: MixerHandle, tap: TapHandle, info: StreamInfo) {
        if let Some(mut c) = self.check.take() {
            c.cal.cancel(&mut self.io.mix, &mut self.io.tap);
            self.finish_check_outcome(&c.cal);
        }
        if self.round.is_some() {
            if !self.paused {
                self.drive(&[JudgeInput::Reset]);
            }
            self.finish_round(true);
        }
        self.close_after_round = false;
        self.close_tape();
        self.io.tap.set_raw(false);
        // The spare raw ring belongs to the old tap.
        self.raw_spare = None;
        self.io.mix = mix;
        self.io.tap = tap;
        self.key = ProfileKey::new(self.cfg.host.clone(), &info);
        self.reopen_lag -= 1;
        self.rewire_epoch = self.max_epoch;
        let floor = if self.reopen_lag < 0 {
            self.max_epoch + 1
        } else {
            self.max_epoch
        };
        self.adapter = Adapter::new(f64::from(info.fs_in), f64::from(info.fs_out));
        self.adapter.set_epoch_floor(floor);
        self.info = info;
        self.snap = None;
        self.outbox.clear();
        self.click_on = false;
        self.unverify();
        self.paused = false;
        if self.record {
            self.open_tape();
        }
        for e in std::mem::take(&mut self.held) {
            self.handle_event(&e);
        }
    }

    // ------------------------------------------------------------ soundcheck

    fn cmd_soundcheck(&mut self, force: bool) {
        if self.paused {
            self.notice(Notice::Refused(Refusal::Paused));
            return;
        }
        if self.round.is_some() {
            // I5: the round is not disturbed.
            self.pending_soundcheck = Some(force || self.pending_soundcheck.unwrap_or(false));
            self.notice(Notice::Deferred(Deferred::Soundcheck));
            return;
        }
        if self.check.is_some() {
            return;
        }
        self.begin_check(force);
    }

    fn begin_check(&mut self, force: bool) {
        let saved = find_profile(&self.profiles, &self.key).cloned();
        if !force
            && self.verified
            && let (Some(p), Some(s)) = (&saved, &self.snap)
            && p.holds_on(s)
        {
            self.emit(SessionEvent::SoundcheckDone {
                basis: p.basis(),
                outcome: SoundOutcome::Kept,
            });
            return;
        }
        self.stop_click();
        let cfg = CalibConfig::short(self.key.clone(), self.cfg.date.clone());
        self.check = Some(Check {
            cal: Calibrator::new(cfg, if force { None } else { saved }),
        });
    }

    fn step_check(&mut self, events: &[EngineEvent]) {
        let Some(c) = self.check.as_mut() else {
            return;
        };
        let up = c.cal.step(&mut self.io.mix, &mut self.io.tap, events);
        if up.changed {
            let _ = self.ev_tx.send(SessionEvent::Prompt(up.prompt));
        }
        if c.cal.is_done() && c.cal.flushed() {
            let Some(c) = self.check.take() else {
                return;
            };
            self.finish_check_outcome(&c.cal);
        }
    }

    fn finish_check_outcome(&mut self, cal: &Calibrator) {
        let (basis, outcome) = match cal.outcome() {
            Some(Outcome::Kept(p)) => (p.basis(), SoundOutcome::Kept),
            Some(Outcome::Calibrated(p)) => (p.basis(), SoundOutcome::Calibrated),
            Some(Outcome::Unstable) => (Basis::None, SoundOutcome::Unstable),
            Some(Outcome::MicBlocked) => (Basis::None, SoundOutcome::MicBlocked),
            Some(Outcome::Interrupted) | None => (Basis::None, SoundOutcome::Interrupted),
        };
        if let Some(Outcome::Kept(p) | Outcome::Calibrated(p)) = cal.outcome() {
            upsert_profile(&mut self.profiles, p.clone());
            self.save_profiles();
            self.verified = true;
            self.device_changed = false;
            if let Some(t) = &self.tape {
                let p = p.clone();
                t.update_header(move |h| h.set_profile(&p));
            }
        } else {
            // A soundcheck that found no usable setup (or was cut off) verifies nothing,
            // whatever an earlier one said.
            self.verified = false;
        }
        self.emit(SessionEvent::SoundcheckDone { basis, outcome });
    }

    fn cmd_cancel_soundcheck(&mut self) {
        let waited = self.pending_soundcheck.take().is_some();
        match self.check.take() {
            Some(mut c) => c.cal.cancel(&mut self.io.mix, &mut self.io.tap),
            None if !waited => return,
            None => {}
        }
        let basis = match (self.verified, self.profile(), &self.snap) {
            (true, Some(p), Some(s)) if p.holds_on(s) => p.basis(),
            _ => Basis::None,
        };
        self.emit(SessionEvent::SoundcheckCancelled { basis });
    }

    // ------------------------------------------------------------ click track

    fn cmd_click(&mut self, bpm: f64, beats: u32, accent_every: u8) {
        if self.paused {
            self.notice(Notice::Refused(Refusal::Paused));
            return;
        }
        if self.round.is_some() {
            self.notice(Notice::Refused(Refusal::RoundRunning));
            return;
        }
        if !(bpm.is_finite() && bpm > 0.0) || beats == 0 {
            self.notice(Notice::Refused(Refusal::BadChart));
            return;
        }
        if self.check.is_some() || self.snap.is_none() {
            self.pending_click = Some((bpm, beats, accent_every));
            self.notice(Notice::Deferred(Deferred::Round));
            return;
        }
        self.start_click(bpm, beats, accent_every);
    }

    fn start_click(&mut self, bpm: f64, beats: u32, accent_every: u8) {
        let Some(s) = self.snap else {
            return;
        };
        self.stop_click();
        let fs_out = f64::from(self.info.fs_out);
        let start = (s.last_out_frame() + START_DELAY_S * fs_out)
            .max(0.0)
            .round() as u64;
        self.outbox.push_back(MixCmd::Metronome {
            tag: CLICK_TAG,
            start,
            period: 60.0 / bpm * fs_out,
            accent_every,
            beats,
            gain: self.cfg.click_gain,
        });
        self.click_on = true;
    }

    fn stop_click(&mut self) {
        if !self.click_on {
            return;
        }
        self.click_on = false;
        let at = self.snap.map_or(0, |s| s.last_out_frame().max(0.0) as u64);
        self.outbox
            .retain(|c| !matches!(c, MixCmd::Metronome { tag, .. } if *tag == CLICK_TAG));
        self.outbox
            .push_back(MixCmd::StopTag { tag: CLICK_TAG, at });
    }

    fn save_profiles(&self) {
        let Some(path) = self.cfg.profiles.as_deref() else {
            return;
        };
        let text = profiles_to_json(&self.profiles);
        let write = || -> io::Result<()> {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            fs::write(&tmp, text)?;
            fs::rename(&tmp, path)
        };
        if write().is_err() {
            self.notice(Notice::ProfilesNotSaved);
        }
    }

    // ------------------------------------------------------------ rounds

    /// The timing reference a round started now freezes, and why it may be missing.
    fn timing_and_advice(&self) -> (TimingRef, Option<Advice>) {
        let Some(p) = find_profile(&self.profiles, &self.key) else {
            return (TimingRef::none(), Some(Advice::NoProfile));
        };
        let Some(s) = &self.snap else {
            return (TimingRef::none(), None);
        };
        if !p.holds_on(s) {
            return (TimingRef::none(), Some(Advice::NotHolding));
        }
        if !self.verified {
            let why = if self.device_changed {
                Advice::DeviceChanged
            } else {
                Advice::NotChecked
            };
            return (TimingRef::none(), Some(why));
        }
        let t = timing_ref(Some(p), Some(s));
        (t, (!t.judges_timing()).then_some(Advice::NoBasis))
    }

    fn cmd_start_round(&mut self, req: PendingRound) {
        if self.paused {
            self.notice(Notice::Refused(Refusal::Paused));
            return;
        }
        if self.round.is_some() {
            self.notice(Notice::Refused(Refusal::RoundRunning));
            return;
        }
        if req.chart.validate().is_err() || pulse::expectations(&req.chart).is_empty() {
            self.notice(Notice::Refused(Refusal::BadChart));
            return;
        }
        if self.check.is_some() || self.snap.is_none() {
            self.pending_round = Some(req);
            self.notice(Notice::Deferred(Deferred::Round));
            return;
        }
        self.start_round(req);
    }

    fn start_round(&mut self, req: PendingRound) {
        let (timing, advice) = self.timing_and_advice();
        if let Some(a) = advice {
            self.notice(Notice::SoundcheckAdvised(a));
        }
        let mut jcfg = JudgeConfig::new(f64::from(self.info.fs_in), self.level);
        jcfg.close_margin_ms += self.cfg.close_margin_extra_ms;
        let n = pulse::expectations(&req.chart).len();
        let judge = PulseJudge::new(jcfg, timing, &self.cfg.gates);
        self.stop_click();
        self.pending_click = None;
        self.finished = None;
        self.round = Some(Round {
            chart: req.chart,
            lead_in: req.lead_in,
            n,
            judge,
            jcfg,
            log: VerdictLog::with_capacity(n),
            timing,
            level: self.level,
            tag: ROUND_TAG,
            attempt: 0,
            anchor: 0,
            segs: Vec::new(),
            next_seg: 0,
            beat_seen: false,
        });
        if let Some(label) = req.take
            && let Some(t) = &self.tape
        {
            t.event(TapeEvent::Take { label });
        }
        self.tape_round_header(timing, jcfg);
        self.arm(0);
    }

    fn tape_round_header(&self, timing: TimingRef, jcfg: JudgeConfig) {
        let Some(t) = &self.tape else {
            return;
        };
        let gates = self.cfg.gates.clone();
        let profile = find_profile(&self.profiles, &self.key).cloned();
        let srcs = self
            .snap
            .as_ref()
            .map(|s| (s.source(Side::In), s.source(Side::Out)));
        t.update_header(move |h| {
            h.timing = timing;
            h.judge = jcfg;
            h.set_gates(&gates);
            if let Some(p) = &profile {
                h.set_profile(p);
            }
            if let Some((a, b)) = srcs {
                h.stamp_in = a;
                h.stamp_out = b;
            }
        });
    }

    /// Places the clicks of the round `attempt` times retried and starts announcing it.
    fn arm(&mut self, attempt: u32) {
        let Some(snap) = self.snap else {
            return;
        };
        let Some(r) = self.round.as_mut() else {
            return;
        };
        let fs_out = f64::from(self.info.fs_out);
        let last = snap.last_out_frame();
        if attempt > 0 {
            // The first clicks of the old attempt came late: take them back.
            let old = r.tag;
            self.outbox
                .retain(|c| !matches!(c, MixCmd::Metronome { tag, .. } if *tag == old));
            self.outbox.push_back(MixCmd::StopTag {
                tag: old,
                at: last.max(0.0) as u64,
            });
        }
        let delay = (START_DELAY_S + 0.25 * f64::from(attempt)) * fs_out;
        let start = (last + delay).max(0.0).round() as u64;
        let period0 = 60.0 / r.chart.bpm_at(0) * fs_out;
        let anchor = start + (f64::from(r.lead_in) * period0).round() as u64;
        r.tag = ROUND_TAG + attempt;
        r.attempt = attempt;
        r.anchor = anchor;
        r.beat_seen = false;
        r.segs = click_segments(&r.chart, anchor, fs_out);
        r.next_seg = 0;
        r.judge = PulseJudge::new(r.jcfg, r.timing, &self.cfg.gates);
        r.log.clear();
        if r.lead_in > 0 {
            self.outbox.push_back(MixCmd::Metronome {
                tag: r.tag,
                start,
                period: period0,
                accent_every: 0,
                beats: r.lead_in,
                gain: self.cfg.click_gain,
            });
        }
        self.adapter.begin(&r.chart, anchor);
        if let Some(t) = &self.tape {
            t.event(TapeEvent::Anchor {
                out_frame: anchor,
                chart_json: r.chart.to_json(),
                timing: Some(r.timing),
                judge: Some(r.jcfg),
            });
        }
        self.emit(SessionEvent::RoundArmed {
            anchor,
            fs_out: self.info.fs_out,
        });
    }

    fn finish_round(&mut self, interrupted: bool) {
        let Some(r) = self.round.take() else {
            return;
        };
        self.adapter.end();
        if interrupted {
            let at = self.snap.map_or(0, |s| s.last_out_frame().max(0.0) as u64);
            self.outbox
                .retain(|c| !matches!(c, MixCmd::Metronome { tag, .. } if *tag == r.tag));
            self.outbox.push_back(MixCmd::StopTag { tag: r.tag, at });
        }
        let stats = r.log.stats(&r.timing).gated(&self.cfg.gates);
        let better = match (&self.prev_stats, interrupted) {
            (Some(prev), false) => stats.improvements(prev),
            _ => Vec::new(),
        };
        let suggest =
            tolerance_suggestion(r.log.verdicts()).filter(|l| l.is_narrower_than(r.level));
        if let Some(t) = &self.tape {
            t.event(TapeEvent::Round(stats));
        }
        if !interrupted {
            self.prev_stats = Some(stats);
        }
        self.finished = Some(r);
        self.emit(SessionEvent::RoundDone {
            stats,
            better,
            suggest,
            interrupted,
        });
        if std::mem::take(&mut self.close_after_round) {
            self.flush_outbox();
            self.close_tape();
        }
    }

    // ------------------------------------------------------------ upkeep

    fn flush_outbox(&mut self) {
        while let Some(c) = self.outbox.front() {
            if self.io.mix.send(*c) {
                self.outbox.pop_front();
            } else {
                break;
            }
        }
    }

    fn upkeep(&mut self) {
        if !self.paused && self.check.is_none() && self.round.is_none() {
            if let Some(req) = self.pending_round.take() {
                if self.snap.is_some() {
                    self.start_round(req);
                } else {
                    self.pending_round = Some(req);
                }
            } else if let Some(force) = self.pending_soundcheck.take() {
                self.begin_check(force);
            } else if self.snap.is_some()
                && let Some((bpm, beats, accent)) = self.pending_click.take()
            {
                self.start_click(bpm, beats, accent);
            }
        }
        if let (Some(r), Some(s)) = (self.round.as_mut(), self.snap) {
            let horizon = s.last_out_frame() + SEGMENT_AHEAD_S * f64::from(self.info.fs_out);
            while let Some(seg) = r.segs.get(r.next_seg) {
                if seg.start as f64 > horizon {
                    break;
                }
                self.outbox.push_back(MixCmd::Metronome {
                    tag: r.tag,
                    start: seg.start,
                    period: seg.period,
                    accent_every: seg.accent,
                    beats: seg.beats,
                    gain: self.cfg.click_gain,
                });
                r.next_seg += 1;
            }
        }
        self.flush_outbox();
        if self
            .round
            .as_ref()
            .is_some_and(|r| r.log.verdicts().len() >= r.n)
        {
            self.finish_round(false);
        }
        self.watch_tape();
    }

    // ------------------------------------------------------------ tapes

    fn open_tape(&mut self) {
        let Some(root) = self.cfg.tapes.clone() else {
            return;
        };
        let mut h = TapeHeader::new(self.info.fs_in, self.info.fs_out);
        h.ch_in = self.info.ch_in;
        h.ch_out = self.info.ch_out;
        h.in_id.clone_from(&self.info.in_id);
        h.out_id.clone_from(&self.info.out_id);
        h.origin_offset_ns = self.info.origin_offset_ns;
        h.video_offset_ms = self.cfg.video_offset_ms;
        h.judge = JudgeConfig::new(f64::from(self.info.fs_in), self.level);
        h.set_gates(&self.cfg.gates);
        if let Some(s) = &self.snap {
            h.stamp_in = s.source(Side::In);
            h.stamp_out = s.source(Side::Out);
        }
        if let Some(p) = find_profile(&self.profiles, &self.key) {
            h.set_profile(p);
        }
        // The ring of an earlier tape of this epoch comes back to the next one. What it may
        // still hold from before is not this tape's: it is emptied while the ring is off.
        let mut spare = self.raw_spare.take();
        if let Some(k) = spare.as_mut() {
            k.discard_pending();
        }
        self.io.tap.set_raw(true);
        let raw = self.io.tap.take_raw();
        let audio = raw.is_some() || spare.is_some();
        let made = new_tape_dir(&root).and_then(|dir| match (raw, spare) {
            (None, Some(k)) => TapeWriter::create_with_kept(&dir, h, k),
            (raw, _) => TapeWriter::create(&dir, h, raw),
        });
        match made {
            Ok(w) => {
                let dir = w.dir().to_path_buf();
                // A tape that starts mid-run still opens with the clock map in force.
                if let Some(s) = &self.snap {
                    w.event(TapeEvent::Engine(EngineEvent::Clock(*s)));
                }
                self.tape = Some(w);
                self.tape_noted = false;
                self.emit(SessionEvent::Tape(TapeNote::Started { dir, audio }));
            }
            Err(e) => {
                self.io.tap.set_raw(false);
                self.emit(SessionEvent::Tape(TapeNote::Failed(e.to_string())));
            }
        }
    }

    fn close_tape(&mut self) {
        let Some(w) = self.tape.take() else {
            return;
        };
        self.io.tap.set_raw(false);
        let (res, raw) = w.finish_keep_raw();
        self.raw_spare = raw;
        match res {
            Ok(sum) => self.emit(SessionEvent::Tape(TapeNote::Closed(sum))),
            Err(e) => self.emit(SessionEvent::Tape(TapeNote::Failed(e.to_string()))),
        }
    }

    /// Tells the UI once if the tape stopped.
    fn watch_tape(&mut self) {
        let Some(w) = &self.tape else {
            return;
        };
        if self.tape_noted {
            return;
        }
        let why = if w.failed() {
            Some("the tape could not be written")
        } else if w.overflowed() {
            Some("the tape fell behind and lost events")
        } else {
            None
        };
        if let Some(why) = why {
            self.tape_noted = true;
            self.emit(SessionEvent::Tape(TapeNote::Failed(why.into())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guitarness_audio::StampSource;
    use guitarness_audio::clock::{ClockParts, ClockQuality};

    fn key() -> ProfileKey {
        ProfileKey {
            host: "fake".into(),
            in_id: "in".into(),
            out_id: "out".into(),
            fs_in: 48_000,
            fs_out: 48_000,
        }
    }

    fn profile(loopback: bool, src: StampSource) -> Profile {
        Profile {
            key: key(),
            l_rt_ms: loopback.then_some(80.0),
            mad_ms: loopback.then_some(0.4),
            tendency_ms: loopback.then_some(-30.0),
            tendency_mad_ms: loopback.then_some(4.0),
            combined_ms: (!loopback).then_some(61.0),
            combined_mad_ms: (!loopback).then_some(5.0),
            combined_n: (!loopback).then_some(12),
            judgeable: true,
            date: "2026-11-16".into(),
            clock_quality: ClockQuality::Locked,
            drift_ppm: 0.0,
            stamps_in: src,
            stamps_out: src,
        }
    }

    fn snap(src: StampSource) -> ClockSnapshot {
        ClockSnapshot::from_parts(ClockParts {
            epoch: 0,
            quality: ClockQuality::Locked,
            n_in: 1000.0,
            t_in: 1.0,
            p_in: 1.0 / 48_000.0,
            n_out: 1000.0,
            t_out: 1.0,
            p_out: 1.0 / 48_000.0,
            fs_in: 48_000.0,
            fs_out: 48_000.0,
            src_in: src,
            src_out: src,
            base_ns: Some(0),
        })
    }

    #[test]
    fn timing_ref_is_the_judge_offset_and_never_the_tendency() {
        let s = snap(StampSource::Device);
        let lb = profile(true, StampSource::Device);
        let t = timing_ref(Some(&lb), Some(&s));
        assert_eq!(t.offset_ms(), lb.judge_offset_ms());
        assert_eq!(t.offset_ms(), Some(80.0), "the tendency (-30) is not in it");
        assert!(t.may_claim_tendency());
        let est = profile(false, StampSource::Device);
        let t = timing_ref(Some(&est), Some(&s));
        assert_eq!(t.offset_ms(), est.judge_offset_ms());
        assert_eq!(t.offset_ms(), Some(61.0));
        assert!(!t.may_claim_tendency());
    }

    #[test]
    fn a_profile_that_does_not_hold_or_is_missing_judges_nothing() {
        let on_callback = snap(StampSource::Callback);
        let lb = profile(true, StampSource::Device);
        assert_eq!(timing_ref(Some(&lb), Some(&on_callback)), TimingRef::none());
        assert_eq!(timing_ref(Some(&lb), None), TimingRef::none());
        assert_eq!(timing_ref(None, Some(&on_callback)), TimingRef::none());
        let mut no_basis = profile(true, StampSource::Device);
        no_basis.judgeable = false;
        let s = snap(StampSource::Device);
        assert_eq!(timing_ref(Some(&no_basis), Some(&s)), TimingRef::none());
    }

    #[test]
    fn click_segments_follow_the_tempo_map() {
        let c = pulse::exercise(pulse::Exercise::OnClickQuarters, 120.0, 2);
        let segs = click_segments(&c, 10_000, 48_000.0);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].start, 10_000);
        assert_eq!(segs[0].beats, 8);
        assert_eq!(segs[0].accent, 4);
        assert!((segs[0].period - 24_000.0).abs() < 1e-9);
    }
}
