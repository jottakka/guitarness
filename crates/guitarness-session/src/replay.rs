//! Replay of a tape and the confusion report (owned by S3).
//!
//! # Levels
//!
//! - [`ReplayLevel::Events`]: the recorded engine events go through a fresh [`Adapter`] and
//!   [`PulseJudge`], round by round, and what the judge says is compared with the recorded
//!   `Out` lines.
//! - [`ReplayLevel::Audio`]: `mic.wav` goes through an [`OnsetDetector`]; its onsets are
//!   compared one by one with the recorded ones ([`ONSET_TOL_MS`]) and the round is judged
//!   again with the new onsets. An onset that matches a recorded one keeps the place it had
//!   on the tape; one that has no match is put before the first recorded `Clock` whose
//!   `last_in_frame` passes the frame at which the detector emitted it.
//!
//! # Rounds
//!
//! A round is the stretch from one `Anchor` line to the next. The judge, the chart and the
//! level are rebuilt at every `Anchor`. The tape header holds the timing reference and the
//! judge settings of the *latest* round only, so they are used for every round (the level
//! follows the `SetLevel` command lines); a tape of rounds with different references
//! replays exactly only if the reference did not change (a note says so).
//!
//! # What counts for a gate
//!
//! Only labeled rounds: those with `truth` lines (the test bench) or a take labeled
//! `silence`, `clicks_only` or `speech`. The tape format has no field that marks a round
//! nobody played (`guitarness audio record` without `--chart` runs a synthetic click chart), so
//! such rounds are recognised only by having no label, and they are left out.

use std::fmt::Write as _;
use std::path::PathBuf;

use guitarness_audio::{EngineEvent, OnsetNote};
use guitarness_core::chart::Chart;
use guitarness_core::gates::GateTable;
use guitarness_core::judge::{JudgeConfig, JudgeCounters, PulseJudge};
use guitarness_core::pulse;
use guitarness_core::report::{ConfusionReport, Observation, Outcome, TapeSource, Truth};
use guitarness_core::stats::{RoundStats, VerdictLog};
use guitarness_core::timing::{Level, TimingBasis, TimingRef};
use guitarness_core::verdict::{
    Attack, Judge, JudgeInput, JudgeOut, PulseVerdict, Timing, VerdictType, Why,
};
use guitarness_dsp::{OnsetConfig, OnsetDetector, OnsetEvent};

use crate::adapter::Adapter;
use crate::format::{SessionCmdLog, TapeError, TapeEvent};
use crate::reader::Tape;
use crate::rig::TruthStrum;

/// Two onsets are the same attack if they differ by no more than this, ms.
pub const ONSET_TOL_MS: f64 = 0.01;
/// The replay feeds the detector in blocks of this many frames.
const BLOCK: usize = 256;
/// Take labels that say every attack in the take is false.
pub const FALSE_ATTACK_LABELS: [&str; 3] = ["silence", "clicks_only", "speech"];
/// A truth within this many ms of a window edge is borderline.
const BORDER_MS: f64 = 5.0;

/// How deep the replay goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayLevel {
    /// Recorded engine events through adapter and judge.
    Events,
    /// The microphone through the onset detector, then the judge.
    Audio,
}

/// What can go wrong.
#[derive(Debug)]
pub enum ReplayError {
    /// The tape could not be read.
    Tape(TapeError),
    /// The tape has no microphone recording.
    NoAudio,
    /// The chart of an `Anchor` line does not parse.
    BadChart {
        /// Round number (0-based).
        round: usize,
        /// The parser's words.
        why: String,
    },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayError::Tape(e) => write!(f, "tape: {e}"),
            ReplayError::NoAudio => write!(f, "the tape has no microphone recording"),
            ReplayError::BadChart { round, why } => {
                write!(f, "round {round}: the chart does not parse: {why}")
            }
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<TapeError> for ReplayError {
    fn from(e: TapeError) -> Self {
        ReplayError::Tape(e)
    }
}

/// Changes to the judge for a what-if replay. `Default` changes nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReplayOptions {
    /// Replace the judge settings of every round (the level included).
    pub judge: Option<JudgeConfig>,
    /// Replace the timing reference of every round.
    pub timing: Option<TimingRef>,
    /// Add this to the offset the judge subtracts, ms (a tampered reference).
    pub offset_shift_ms: f64,
}

fn shifted(t: TimingRef, by: f64) -> TimingRef {
    match t.basis {
        TimingBasis::Loopback { l_rt_ms, mad_ms } => TimingRef::loopback(l_rt_ms + by, mad_ms),
        TimingBasis::Estimate {
            offset_ms,
            mad_ms,
            n,
        } => TimingRef::estimate(offset_ms + by, mad_ms, n),
        TimingBasis::None => t,
    }
}

/// One judge output that differs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Diff {
    /// Round (0-based).
    pub round: usize,
    /// Event the output is about, when it is about one.
    pub id: Option<u32>,
    /// What the tape has (`None`: the replay made an output the tape lacks).
    pub recorded: Option<JudgeOut>,
    /// What the replay made (`None`: the tape has an output the replay lacks).
    pub replayed: Option<JudgeOut>,
}

/// One round, recorded and replayed.
#[derive(Clone, Debug)]
pub struct RoundRun {
    /// Round number (0-based).
    pub index: usize,
    /// Output frame of chart time zero.
    pub anchor: u64,
    /// The chart.
    pub chart: Chart,
    /// Level the judge ran at.
    pub level: Level,
    /// Judge settings used.
    pub config: JudgeConfig,
    /// Timing reference used.
    pub timing: TimingRef,
    /// Label of the take this round belongs to.
    pub take: Option<String>,
    /// Outputs on the tape.
    pub recorded: Vec<JudgeOut>,
    /// Outputs of the replay.
    pub replayed: Vec<JudgeOut>,
    /// The `Round` line was seen (the round ended on the tape).
    pub finished: bool,
    /// Statistics on the tape (the last `Round` line).
    pub recorded_stats: Option<RoundStats>,
    /// Statistics of the replayed outputs.
    pub replayed_stats: RoundStats,
    /// What the replayed judge counted as faults.
    pub counters: JudgeCounters,
    /// What the virtual player really did in this round (test bench tapes).
    pub truth: Vec<TruthStrum>,
}

/// Onsets of the detector against those on the tape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnsetCheck {
    /// Onsets on the tape (with a frame number).
    pub recorded: usize,
    /// Onsets the detector found now.
    pub found: usize,
    /// Pairs within the tolerance.
    pub matched: usize,
    /// Input frames of tape onsets the detector did not find.
    pub missing: Vec<f64>,
    /// Input frames of onsets the detector found and the tape lacks.
    pub extra: Vec<f64>,
    /// Largest distance of a matched pair, ms.
    pub worst_ms: f64,
}

impl OnsetCheck {
    /// Same onsets, same places.
    pub fn is_exact(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty()
    }
}

/// What a replay found.
#[derive(Clone, Debug)]
pub struct ReplayResult {
    /// The level that ran.
    pub level: ReplayLevel,
    /// Every round of the tape.
    pub rounds: Vec<RoundRun>,
    /// Outputs that differ, in round order.
    pub diffs: Vec<Diff>,
    /// Onset comparison (`Audio` only).
    pub onsets: Option<OnsetCheck>,
    /// Things the reader should know (approximations).
    pub notes: Vec<String>,
    /// The tape is complete: it may enter a gate.
    pub tape_complete: bool,
}

impl ReplayResult {
    /// No output differs and (at `Audio`) no onset differs.
    pub fn is_exact(&self) -> bool {
        self.diffs.is_empty() && self.onsets.as_ref().is_none_or(OnsetCheck::is_exact)
    }
}

/// Replays `tape` at `level` with the judge as it was recorded.
pub fn replay(
    tape: &Tape,
    level: ReplayLevel,
    gates: &GateTable,
) -> Result<ReplayResult, ReplayError> {
    replay_with(tape, level, gates, &ReplayOptions::default())
}

/// Replays `tape` with some of the judge's settings changed.
pub fn replay_with(
    tape: &Tape,
    level: ReplayLevel,
    gates: &GateTable,
    opts: &ReplayOptions,
) -> Result<ReplayResult, ReplayError> {
    let mut notes = Vec::new();
    let mut events: Vec<TapeEvent> = tape.events.iter().map(|(_, e)| e.clone()).collect();
    let mut onsets = None;
    if level == ReplayLevel::Audio {
        let (ev, check) = rebuild_with_audio(tape, events, &mut notes)?;
        events = ev;
        onsets = Some(check);
    }
    let rounds = run_events(tape, &events, gates, opts, &mut notes)?;
    let mut diffs = Vec::new();
    for r in &rounds {
        diff_round(r, &mut diffs);
    }
    Ok(ReplayResult {
        level,
        rounds,
        diffs,
        onsets,
        notes,
        tape_complete: tape.complete,
    })
}

// ---------------------------------------------------------------------------------------
// The judge side
// ---------------------------------------------------------------------------------------

struct Live {
    judge: PulseJudge,
    log: VerdictLog,
    active: bool,
}

fn run_events(
    tape: &Tape,
    events: &[TapeEvent],
    gates: &GateTable,
    opts: &ReplayOptions,
    notes: &mut Vec<String>,
) -> Result<Vec<RoundRun>, ReplayError> {
    let h = &tape.header;
    let mut adapter = Adapter::new(f64::from(h.fs_in), f64::from(h.fs_out));
    let has_set_level = events
        .iter()
        .any(|e| matches!(e, TapeEvent::Cmd(SessionCmdLog::SetLevel(_))));
    let mut level = if has_set_level {
        Level::Apprentice
    } else {
        h.judge.level
    };
    let mut take: Option<String> = None;
    let mut header_used = 0usize;
    let mut rounds: Vec<RoundRun> = Vec::new();
    let mut live: Option<Live> = None;
    let mut ins: Vec<JudgeInput> = Vec::new();
    let mut outs: Vec<JudgeOut> = Vec::new();

    fn feed(live: &mut Live, run: &mut RoundRun, ins: &[JudgeInput], outs: &mut Vec<JudgeOut>) {
        outs.clear();
        for i in ins {
            live.judge.push(i, outs);
        }
        for o in outs.iter() {
            live.log.push(o);
            run.replayed.push(*o);
        }
    }

    for ev in events {
        match ev {
            TapeEvent::Engine(e) => {
                ins.clear();
                adapter.on_engine(e, &mut ins);
                if let (Some(l), Some(r)) = (live.as_mut(), rounds.last_mut())
                    && l.active
                {
                    feed(l, r, &ins, &mut outs);
                }
            }
            TapeEvent::Cmd(SessionCmdLog::SetLevel(l)) => level = *l,
            TapeEvent::Cmd(SessionCmdLog::Dispute) => {
                if let (Some(l), Some(r)) = (live.as_mut(), rounds.last_mut()) {
                    feed(l, r, &[JudgeInput::Dispute], &mut outs);
                }
            }
            TapeEvent::Cmd(SessionCmdLog::Rewire) => {
                if let (Some(l), Some(r)) = (live.as_mut(), rounds.last_mut())
                    && l.active
                {
                    feed(l, r, &[JudgeInput::Reset], &mut outs);
                }
            }
            TapeEvent::Take { label } => take = Some(label.clone()),
            TapeEvent::Anchor {
                out_frame,
                chart_json,
                timing: a_timing,
                judge: a_judge,
            } => {
                let index = rounds.len();
                let chart = Chart::from_json(chart_json).map_err(|e| ReplayError::BadChart {
                    round: index,
                    why: format!("{e:?}"),
                })?;
                // The round's own reference from its Anchor; the header (latest round) only
                // for tapes that predate the field.
                if a_timing.is_none() || a_judge.is_none() {
                    header_used += 1;
                }
                let mut config = opts
                    .judge
                    .or(*a_judge)
                    .unwrap_or(JudgeConfig { level, ..h.judge });
                config.fs_in = h.judge.fs_in;
                let timing = shifted(
                    opts.timing.or(*a_timing).unwrap_or(h.timing),
                    opts.offset_shift_ms,
                );
                adapter.begin(&chart, *out_frame);
                let n = pulse::expectations(&chart).len();
                live = Some(Live {
                    judge: PulseJudge::new(config, timing, gates),
                    log: VerdictLog::with_capacity(n),
                    active: true,
                });
                rounds.push(RoundRun {
                    index,
                    anchor: *out_frame,
                    chart,
                    level: config.level,
                    config,
                    timing,
                    take: take.clone(),
                    recorded: Vec::new(),
                    replayed: Vec::new(),
                    finished: false,
                    recorded_stats: None,
                    replayed_stats: VerdictLog::new().stats(&timing),
                    counters: JudgeCounters::default(),
                    truth: Vec::new(),
                });
            }
            TapeEvent::Out(o) => {
                if let Some(r) = rounds.last_mut() {
                    r.recorded.push(*o);
                }
            }
            TapeEvent::Round(stats) => {
                if let Some(r) = rounds.last_mut() {
                    r.recorded_stats = Some(*stats);
                    if let Some(l) = live.as_mut()
                        && l.active
                    {
                        l.active = false;
                        r.finished = true;
                        adapter.end();
                    }
                }
            }
            TapeEvent::Truth(t) => {
                // The round it belongs to: the latest anchor at or before its click.
                if let Some(r) = rounds.iter_mut().rev().find(|r| r.anchor <= t.out_frame) {
                    r.truth.push(t.clone());
                }
            }
            _ => {}
        }
        if let (Some(l), Some(r)) = (live.as_ref(), rounds.last_mut()) {
            r.counters = l.judge.counters();
            r.replayed_stats = l.log.stats(&r.timing).gated(gates);
        }
    }
    if header_used > 1 && opts.timing.is_none() {
        notes.push(
            "the header holds the timing reference of the latest round only; it was used for the rounds whose Anchor has none"
                .into(),
        );
    }
    if has_set_level && header_used > 0 {
        notes.push(
            "the level before the first SetLevel line is not on the tape; Apprentice was assumed"
                .into(),
        );
    }
    Ok(rounds)
}

/// Positions of the differing outputs of one round: a longest-common-subsequence diff, the
/// removed and added runs between two common outputs paired in order.
fn diff_round(r: &RoundRun, diffs: &mut Vec<Diff>) {
    let key = |o: &JudgeOut| format!("{o:?}");
    let a: Vec<String> = r.recorded.iter().map(key).collect();
    let b: Vec<String> = r.replayed.iter().map(key).collect();
    let (n, m) = (a.len(), b.len());
    // Trim the common head and tail so a small diff costs little.
    let mut head = 0;
    while head < n && head < m && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < n - head && tail < m - head && a[n - 1 - tail] == b[m - 1 - tail] {
        tail += 1;
    }
    let (a, b) = (&a[head..n - tail], &b[head..m - tail]);
    let (ra, rb) = (&r.recorded[head..n - tail], &r.replayed[head..m - tail]);
    let (n, m) = (a.len(), b.len());
    let id_of = |o: &JudgeOut| match o {
        JudgeOut::Flash { id, .. } => *id,
        JudgeOut::Verdict(v) => Some(v.id),
        JudgeOut::Withdrawn { id, .. } => Some(*id),
        JudgeOut::Extra { .. } => None,
    };
    let mut push = |x: Option<&JudgeOut>, y: Option<&JudgeOut>| {
        diffs.push(Diff {
            round: r.index,
            id: x.and_then(id_of).or_else(|| y.and_then(id_of)),
            recorded: x.copied(),
            replayed: y.copied(),
        });
    };
    if n.saturating_mul(m) > 16_000_000 {
        for i in 0..n.max(m) {
            push(ra.get(i), rb.get(i));
        }
        return;
    }
    // lcs[i][j]: length of the common subsequence of a[i..] and b[j..].
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if a[i] == b[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let (mut gone, mut came): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    let mut flush = |gone: &mut Vec<usize>, came: &mut Vec<usize>| {
        for k in 0..gone.len().max(came.len()) {
            push(gone.get(k).map(|&x| &ra[x]), came.get(k).map(|&y| &rb[y]));
        }
        gone.clear();
        came.clear();
    };
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            flush(&mut gone, &mut came);
            i += 1;
            j += 1;
        } else if j >= m || (i < n && lcs[at(i + 1, j)] >= lcs[at(i, j + 1)]) {
            gone.push(i);
            i += 1;
        } else {
            came.push(j);
            j += 1;
        }
    }
    flush(&mut gone, &mut came);
}

// ---------------------------------------------------------------------------------------
// The audio side
// ---------------------------------------------------------------------------------------

struct Found {
    event: OnsetEvent,
    /// Input frame at which the detector emitted it.
    emitted_in: f64,
}

fn detect(tape: &Tape, notes: &mut Vec<String>) -> Result<Vec<Found>, ReplayError> {
    if !tape.has_audio() {
        return Err(ReplayError::NoAudio);
    }
    let mic = tape.mic()?;
    let origin = tape.det_origin().unwrap_or(0);
    if origin != 0 {
        notes.push(
            "the recording starts after the detector did: its state differs from the live one"
                .into(),
        );
    }
    if !tape.complete {
        notes.push("the tape is incomplete: the audio replay may not match".into());
    }
    let mut det = OnsetDetector::new(f64::from(tape.header.fs_in), OnsetConfig::default());
    let mut found = Vec::new();
    let mut done = 0usize;
    for block in mic.chunks(BLOCK) {
        done += block.len();
        let at = tape
            .in_of_det((origin + done as u64) as f64)
            .unwrap_or(f64::NAN);
        det.process(block, |e| {
            found.push(Found {
                event: shift(e, origin as f64),
                emitted_in: at,
            });
        });
    }
    let at = tape
        .in_of_det((origin + done as u64) as f64)
        .unwrap_or(f64::NAN);
    det.flush(|e| {
        found.push(Found {
            event: shift(e, origin as f64),
            emitted_in: at,
        });
    });
    Ok(found)
}

fn shift(e: OnsetEvent, by: f64) -> OnsetEvent {
    match e {
        OnsetEvent::Heard { t, at } => OnsetEvent::Heard {
            t: t + by,
            at: at + by,
        },
        OnsetEvent::Strum(mut s) => {
            s.t += by;
            OnsetEvent::Strum(s)
        }
    }
}

fn same_kind(a: &OnsetEvent, b: &OnsetEvent) -> bool {
    matches!(
        (a, b),
        (OnsetEvent::Heard { .. }, OnsetEvent::Heard { .. })
            | (OnsetEvent::Strum(_), OnsetEvent::Strum(_))
    )
}

fn rebuild_with_audio(
    tape: &Tape,
    events: Vec<TapeEvent>,
    notes: &mut Vec<String>,
) -> Result<(Vec<TapeEvent>, OnsetCheck), ReplayError> {
    let found = detect(tape, notes)?;
    let fs = f64::from(tape.header.fs_in);
    let tol = ONSET_TOL_MS * 1.0e-3 * fs;
    let t_in_of = |e: &OnsetEvent| {
        let t = match e {
            OnsetEvent::Heard { t, .. } => *t,
            OnsetEvent::Strum(s) => s.t,
        };
        tape.in_of_det(t).unwrap_or(f64::NAN)
    };
    // The recorded onsets that have a frame, in order, with their line index.
    let rec: Vec<(usize, OnsetNote)> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            TapeEvent::Engine(EngineEvent::Onset(n)) if n.t_in.is_finite() => Some((i, *n)),
            _ => None,
        })
        .collect();
    let mut check = OnsetCheck {
        recorded: rec.len(),
        found: found.len(),
        ..OnsetCheck::default()
    };
    // Pair them in time order.
    let mut pair: Vec<Option<usize>> = vec![None; rec.len()];
    let mut used = vec![false; found.len()];
    let (mut i, mut j) = (0, 0);
    while i < rec.len() && j < found.len() {
        let (tr, tf) = (rec[i].1.t_in, t_in_of(&found[j].event));
        if (tr - tf).abs() <= tol && same_kind(&rec[i].1.event, &found[j].event) {
            pair[i] = Some(j);
            used[j] = true;
            check.matched += 1;
            check.worst_ms = check.worst_ms.max((tr - tf).abs() / fs * 1.0e3);
            i += 1;
            j += 1;
        } else if tr < tf {
            i += 1;
        } else {
            j += 1;
        }
    }
    check.missing = rec
        .iter()
        .zip(&pair)
        .filter(|(_, p)| p.is_none())
        .map(|((_, n), _)| n.t_in)
        .collect();
    check.extra = found
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|(f, _)| t_in_of(&f.event))
        .collect();

    // The new event stream.
    let mut replace: std::collections::HashMap<usize, Option<OnsetNote>> =
        std::collections::HashMap::new();
    for ((line, old), p) in rec.iter().zip(&pair) {
        match p {
            Some(j) => {
                let ev = found[*j].event;
                let at_in = match ev {
                    OnsetEvent::Heard { at, .. } => tape.in_of_det(at),
                    OnsetEvent::Strum(_) => None,
                };
                replace.insert(
                    *line,
                    Some(OnsetNote {
                        event: ev,
                        t_in: t_in_of(&ev),
                        at_in,
                        ..*old
                    }),
                );
            }
            None => {
                replace.insert(*line, None);
            }
        }
    }
    // Unmatched new onsets, by the frame they were emitted at.
    let mut before: Vec<(usize, OnsetEvent, f64)> = Vec::new();
    for (j, f) in found.iter().enumerate() {
        if !used[j] {
            let line = events
                .iter()
                .position(|e| match e {
                    TapeEvent::Engine(EngineEvent::Clock(s)) => s.last_in_frame() > f.emitted_in,
                    _ => false,
                })
                .unwrap_or(events.len());
            before.push((line, f.event, f.emitted_in));
        }
    }
    let mut out = Vec::with_capacity(events.len() + before.len());
    let mut windows: Vec<(f64, f64)> = Vec::new();
    let mut locked = false;
    let mut epoch = 0u32;
    let emit_new = |out: &mut Vec<TapeEvent>,
                    line: usize,
                    windows: &[(f64, f64)],
                    locked: bool,
                    epoch: u32| {
        for (l, ev, _) in &before {
            if *l == line {
                let t_in = t_in_of(ev);
                let at_in = match ev {
                    OnsetEvent::Heard { at, .. } => tape.in_of_det(*at),
                    OnsetEvent::Strum(_) => None,
                };
                let judged = locked
                    && t_in.is_finite()
                    && !windows.iter().any(|&(lo, hi)| t_in >= lo && t_in <= hi);
                out.push(TapeEvent::Engine(EngineEvent::Onset(OnsetNote {
                    event: *ev,
                    t_in,
                    at_in,
                    judged,
                    epoch,
                })));
            }
        }
    };
    for (k, e) in events.into_iter().enumerate() {
        emit_new(&mut out, k, &windows, locked, epoch);
        match &e {
            TapeEvent::Engine(EngineEvent::Clock(s)) => {
                locked = s.quality() == guitarness_audio::ClockQuality::Locked;
                epoch = s.epoch();
            }
            TapeEvent::Engine(EngineEvent::Unjudged { from_in, to_in, .. }) => {
                windows.push((*from_in, *to_in));
            }
            TapeEvent::Engine(EngineEvent::Onset(_)) => match replace.get(&k) {
                Some(Some(n)) => {
                    out.push(TapeEvent::Engine(EngineEvent::Onset(*n)));
                    continue;
                }
                Some(None) => continue,
                None => {}
            },
            _ => {}
        }
        out.push(e);
    }
    let end = out.len();
    let _ = end;
    // New onsets after the last line.
    let last = before.iter().filter(|(l, _, _)| *l >= out.len()).count();
    if last > 0 {
        let n_lines = before.iter().map(|(l, _, _)| *l).max().unwrap_or(0);
        emit_new(&mut out, n_lines, &windows, locked, epoch);
    }
    Ok((out, check))
}

// ---------------------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------------------

/// Which outputs the observations are made from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// What the tape says the judge said.
    Recorded,
    /// What a replay says.
    Replayed,
}

/// A tape that did not enter the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excluded {
    /// Index in the slice given.
    pub index: usize,
    /// The tape folder.
    pub dir: PathBuf,
    /// Why.
    pub why: String,
}

/// A confusion report and the tapes left out of it.
#[derive(Clone, Debug)]
pub struct Evidence {
    /// The counts.
    pub report: ConfusionReport,
    /// Indices of the tapes counted.
    pub used: Vec<usize>,
    /// Tapes left out, with the reason.
    pub excluded: Vec<Excluded>,
    /// Rounds left out of counted tapes because nobody labeled them.
    pub unlabeled_rounds: usize,
}

impl Evidence {
    /// The report as text, then the tapes and rounds that did not count.
    pub fn to_text(&self) -> String {
        let mut s = self.report.to_text();
        let _ = writeln!(
            s,
            "tapes counted: {}; left out: {}; unlabeled rounds left out: {}",
            self.used.len(),
            self.excluded.len(),
            self.unlabeled_rounds
        );
        for x in &self.excluded {
            let _ = writeln!(s, "left out: {} ({})", x.dir.display(), x.why);
        }
        s
    }
}

/// The confusion report of `tapes`, from what the tapes recorded, against the builtin
/// gates. Incomplete tapes and unlabeled rounds do not count.
pub fn report(tapes: &[Tape]) -> ConfusionReport {
    evidence(tapes, &GateTable::builtin()).report
}

/// As [`report`], with the tapes left out named, against `gates`.
pub fn evidence(tapes: &[Tape], gates: &GateTable) -> Evidence {
    evidence_with(tapes, gates, &ReplayOptions::default(), Side::Recorded)
}

/// The report of `tapes` as `opts` would have judged them.
pub fn evidence_replayed(tapes: &[Tape], gates: &GateTable, opts: &ReplayOptions) -> Evidence {
    evidence_with(tapes, gates, opts, Side::Replayed)
}

fn evidence_with(tapes: &[Tape], gates: &GateTable, opts: &ReplayOptions, side: Side) -> Evidence {
    let mut obs = Vec::new();
    let mut used = Vec::new();
    let mut excluded = Vec::new();
    let mut unlabeled = 0;
    for (i, t) in tapes.iter().enumerate() {
        let mut leave = |why: String| {
            excluded.push(Excluded {
                index: i,
                dir: t.dir().to_path_buf(),
                why,
            });
        };
        if !t.complete {
            leave("incomplete tape (not closed cleanly, or something was lost)".into());
            continue;
        }
        match replay_with(t, ReplayLevel::Events, gates, opts) {
            Ok(r) => {
                let (o, skipped) = observations(t, &r, side, i as u32);
                unlabeled += skipped;
                obs.extend(o);
                used.push(i);
            }
            Err(e) => leave(e.to_string()),
        }
    }
    Evidence {
        report: ConfusionReport::build(&obs, gates),
        used,
        excluded,
        unlabeled_rounds: unlabeled,
    }
}

/// The observations of the labeled rounds of one replayed tape, and the number of rounds
/// left out for having no label.
pub fn observations(
    tape: &Tape,
    result: &ReplayResult,
    side: Side,
    tape_id: u32,
) -> (Vec<Observation>, usize) {
    let _ = tape;
    let mut obs = Vec::new();
    let mut skipped = 0;
    for r in &result.rounds {
        let false_take = r
            .take
            .as_deref()
            .is_some_and(|l| FALSE_ATTACK_LABELS.contains(&l));
        let source = if !r.truth.is_empty() {
            TapeSource::Synthetic
        } else if false_take {
            TapeSource::Real
        } else {
            skipped += 1;
            continue;
        };
        let outs = match side {
            Side::Recorded => &r.recorded,
            Side::Replayed => &r.replayed,
        };
        round_observations(r, outs, false_take, source, tape_id, &mut obs);
    }
    (obs, skipped)
}

/// What the judge finally said about one event.
struct Final {
    verdict: PulseVerdict,
    disputed: bool,
}

fn round_observations(
    r: &RoundRun,
    outs: &[JudgeOut],
    false_take: bool,
    source: TapeSource,
    tape: u32,
    obs: &mut Vec<Observation>,
) {
    let mut fin: std::collections::BTreeMap<u32, Final> = std::collections::BTreeMap::new();
    for o in outs {
        match o {
            JudgeOut::Verdict(v) if v.fin => {
                fin.insert(
                    v.id,
                    Final {
                        verdict: *v,
                        disputed: false,
                    },
                );
            }
            JudgeOut::Withdrawn {
                id,
                why: Why::Disputed,
            } => {
                if let Some(f) = fin.get_mut(id) {
                    f.disputed = true;
                }
            }
            _ => {}
        }
    }
    let exps = pulse::expectations(&r.chart);
    let probe = PulseJudge::new(r.config, r.timing, &GateTable::builtin());
    for (id, f) in &fin {
        let Some(e) = exps.iter().find(|e| e.id == *id) else {
            continue;
        };
        if e.ghost {
            continue;
        }
        let shape = probe.shape(e.prev_gap_s, e.next_gap_s);
        // The truth about this event, from the virtual player's lines.
        let truth_line = r.truth.iter().find(|t| {
            !t.extra
                && t.id.is_some_and(|h| {
                    crate::adapter::event_index_of(&r.chart, *id) == Some(h as usize)
                })
        });
        let (attack_truth, timing_truth, a_border, t_border) = if false_take {
            (Truth::Wrong, Truth::Unlabeled, false, false)
        } else if let Some(t) = truth_line {
            match (t.played, t.dt_ms) {
                (true, Some(dt)) => {
                    let a = dt.abs();
                    (
                        if a <= shape.pair_ms {
                            Truth::Right
                        } else {
                            Truth::Wrong
                        },
                        if a <= shape.window_ms {
                            Truth::Right
                        } else {
                            Truth::Wrong
                        },
                        (a - shape.pair_ms).abs() <= BORDER_MS,
                        (a - shape.window_ms).abs() <= BORDER_MS,
                    )
                }
                _ => (Truth::Wrong, Truth::Unlabeled, false, false),
            }
        } else {
            (Truth::Unlabeled, Truth::Unlabeled, false, false)
        };
        let v = &f.verdict;
        let a_out = if f.disputed {
            Outcome::Disputed
        } else {
            match v.attack {
                Attack::Heard => Outcome::Ok,
                Attack::NotHeard => Outcome::NotHeard,
                Attack::NotJudged(_) => Outcome::NotJudged,
            }
        };
        let t_out = if f.disputed {
            Outcome::Disputed
        } else {
            match v.timing {
                Timing::Center | Timing::InWindow => Outcome::Ok,
                Timing::Outside => Outcome::Wrong,
                Timing::NotJudged(_) => Outcome::NotJudged,
            }
        };
        obs.push(Observation {
            vtype: VerdictType::Attack,
            outcome: a_out,
            truth: attack_truth,
            borderline: a_border,
            source,
            tape,
        });
        obs.push(Observation {
            vtype: VerdictType::Timing,
            outcome: t_out,
            truth: timing_truth,
            borderline: t_border,
            source,
            tape,
        });
    }
}
