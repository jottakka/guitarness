//! W1: the learner model inside the game (plan W1, rules N1-N4).
//!
//! Round end -> `evidence_of_round` -> [`BlockRecord`] -> books ([`Learner::ledger`] for the
//! offer, [`Learner::display`] for the numbers the player sees). The play button plays the
//! offer behind it ([`Learner::offer`]); key `O` asks for something else. There is one offer
//! at a time and one offer surface: the Pulse tempo chip (see `pulse::flow`). A probe block
//! builds no summary ([`Learner::show_summary`]).
//!
//! Frame rules: no systems here do file I/O or block. Log lines and the display book go to a
//! writer thread ([`Writer`]); the Guest profile has no writer and never touches the disk.
//! Opening a stored profile ([`Learner::open_local`]) reads files once, at start-up, outside
//! any system.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;

use guitarness_learn::display::DisplayLedger;
use guitarness_learn::evidence::evidence_of_round;
use guitarness_learn::mastery::LearnerLedger;
use guitarness_learn::picker::{next_offer, something_else_in};
use guitarness_learn::probe::choice_for_else;
use guitarness_learn::store::{Store, create_local, open_profile};
use guitarness_learn::types::{
    Axis, BlockKind, BlockPlan, BlockRecord, BlockSpec, Choice, LearnConfig, LogLine, Offer,
    ProfileId, Provenance, SessionCtx,
};
use tonica_core::chart::Chart;
use tonica_core::gates::GateTable;
use tonica_core::pulse::Exercise;
use tonica_core::verdict::{JudgeOut, VerdictType, Why};
use tonica_session::session::SessionEvent;

use crate::AppConfig;
use crate::audio::{AudioCommand, AudioNote};

/// Ticks of one 4/4 bar in the exercise charts.
const BAR_TICKS: u32 = 1920;
/// The key that asks for another offer ("outra coisa").
pub const ELSE_KEY: KeyCode = KeyCode::KeyO;

/// Systems that fold a finished round into the books. Readers of the books (the summary, the
/// flow) run after this set.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub struct LearnSet;

// ------------------------------------------------------------------ writer

enum Job {
    Line(LogLine),
    Display(Box<DisplayLedger>),
    Flush(Sender<()>),
}

/// The off-frame writer: a thread that owns the opened [`Store`]. Systems only send jobs.
pub struct Writer {
    tx: Option<Sender<Job>>,
    handle: Option<JoinHandle<()>>,
    errors: Arc<AtomicU32>,
}

impl Writer {
    fn spawn(mut store: Store) -> Self {
        let (tx, rx) = channel::<Job>();
        let errors = Arc::new(AtomicU32::new(0));
        let err = errors.clone();
        let handle = std::thread::Builder::new()
            .name("learn-writer".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let r = match job {
                        Job::Line(l) => store.append(&l),
                        Job::Display(d) => store.save_display(&d),
                        Job::Flush(ack) => {
                            let _ = ack.send(());
                            Ok(())
                        }
                    };
                    if r.is_err() {
                        err.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
            .ok();
        Self {
            tx: Some(tx),
            handle,
            errors,
        }
    }

    fn send(&self, j: Job) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(j);
        }
    }
}

impl Drop for Writer {
    /// Closing the channel lets the thread finish what is queued; wait for it so nothing is lost.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ------------------------------------------------------------------ state

/// The block being played (or just played).
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveBlock {
    pub kind: BlockKind,
    pub spec: BlockSpec,
    pub after_gap: bool,
    pub ts_ms: i64,
}

/// The learner's state. Starts as the Guest profile (in memory, no disk).
#[derive(Resource)]
pub struct Learner {
    pub cfg: LearnConfig,
    /// In-memory mirror of the log (what the books fold). The file, if any, belongs to the
    /// writer thread.
    pub store: Store,
    pub ledger: LearnerLedger,
    /// Display book: monotone only; the summary counter reads this, never `ledger`.
    pub display: DisplayLedger,
    /// The offer behind the play button, if one was computed.
    pub offer: Option<Offer>,
    /// Counters for `SessionCtx`.
    pub blocks_this_session: u32,
    pub probe_done: bool,
    pub offer_no: u64,
    /// Bumped whenever `offer` changes; the flow re-applies it to the screen.
    pub offer_rev: u32,
    /// The `offer_rev` the screen shows.
    pub applied_rev: u32,
    /// Bars of the block that Play will start.
    pub bars: u8,
    /// Tempo change of the offer as `(old, new)` BPM, while the chip is on the table.
    pub tempo_pair: Option<(u16, u16)>,
    /// False while the last block was a probe: its result is never shown.
    pub show_summary: bool,
    pub active: Option<ActiveBlock>,
    pub gates: GateTable,
    pub basis: String,
    /// A fixed clock (tests, replays); otherwise the wall clock.
    pub clock_ms: Option<i64>,
    session: u32,
    offer_ctx: Option<SessionCtx>,
    chart: Option<Chart>,
    outs: Vec<JudgeOut>,
    /// `ts_ms` of the last block written, for disputes that arrive after the round.
    finished_ts: Option<i64>,
    writer: Option<Writer>,
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn bars_of(chart: &Chart) -> u8 {
    let end = chart.events().last().map_or(0, |e| e.tick + e.dur);
    end.div_ceil(BAR_TICKS).clamp(1, 255) as u8
}

fn exercise_of(chart: &Chart) -> Exercise {
    if chart.id == "on-click-eighths" {
        Exercise::OnClickEighths
    } else {
        Exercise::OnClickQuarters
    }
}

impl Learner {
    pub fn guest() -> Self {
        let cfg = LearnConfig::default();
        Self {
            ledger: LearnerLedger::new(&cfg),
            display: DisplayLedger::new(),
            store: Store::memory(),
            bars: cfg.starter.bars,
            cfg,
            offer: None,
            blocks_this_session: 0,
            probe_done: false,
            offer_no: 0,
            offer_rev: 0,
            applied_rev: 0,
            tempo_pair: None,
            show_summary: true,
            active: None,
            gates: GateTable::builtin(),
            basis: "none".into(),
            clock_ms: None,
            session: (wall_ms() / 1000) as u32,
            offer_ctx: None,
            chart: None,
            outs: Vec::new(),
            finished_ts: None,
            writer: None,
        }
    }

    /// A stored profile. Reads the log and the display book once; call it at start-up, never
    /// from a system. Later writes go to the writer thread.
    pub fn open_local(data: &Path, id: &ProfileId) -> io::Result<Self> {
        let store = open_profile(data, id)?;
        let mut l = Self::guest();
        let lines: Vec<LogLine> = store.lines().to_vec();
        l.ledger = LearnerLedger::fold(&lines, &l.cfg);
        // Rebuild the display book from the blocks, then keep the larger of it and the file.
        let mut replay = LearnerLedger::new(&l.cfg);
        let mut rebuilt = DisplayLedger::new();
        for line in &lines {
            if let LogLine::Block(b) = line {
                let grades = replay.apply(b, &l.cfg);
                rebuilt.apply(b, &grades);
            }
        }
        if let Some(saved) = store.load_display() {
            rebuilt.merge(&saved);
        }
        l.display = rebuilt;
        for line in &lines {
            let _ = l.store.append(line);
        }
        l.writer = store.is_persistent().then(|| Writer::spawn(store));
        Ok(l)
    }

    /// Whether anything is written to disk.
    pub fn is_persistent(&self) -> bool {
        self.writer.is_some()
    }

    /// Writes that failed in the writer thread (never shown).
    pub fn write_errors(&self) -> u32 {
        self.writer
            .as_ref()
            .map_or(0, |w| w.errors.load(Ordering::Relaxed))
    }

    /// Wait until everything queued is on disk. Tests and shutdown only; never a system.
    pub fn flush(&self) {
        if let Some(w) = &self.writer {
            let (ack, rx) = mpsc::channel();
            w.send(Job::Flush(ack));
            let _ = rx.recv();
        }
    }

    pub fn now_ms(&self) -> i64 {
        self.clock_ms.unwrap_or_else(wall_ms)
    }

    fn log(&mut self, line: LogLine) {
        let _ = self.store.append(&line);
        if let Some(w) = &self.writer {
            w.send(Job::Line(line));
        }
    }

    fn save_display(&self) {
        if let Some(w) = &self.writer {
            w.send(Job::Display(Box::new(self.display.clone())));
        }
    }

    fn ctx(&self, now_ms: i64) -> SessionCtx {
        SessionCtx {
            now_ms,
            blocks_this_session: self.blocks_this_session,
            probe_done: self.probe_done,
            offer_no: self.offer_no,
        }
    }

    // -------------------------------------------------------------- the offer

    /// Make sure an offer exists. Returns true when a new one was computed.
    pub fn ensure_offer(&mut self) -> bool {
        if self.offer.is_some() {
            return false;
        }
        let ctx = self.ctx(self.now_ms());
        let offer = next_offer(&self.ledger, &ctx, &self.cfg);
        self.offer_no += 1;
        self.set_offer(offer, ctx);
        true
    }

    fn set_offer(&mut self, offer: Offer, ctx: SessionCtx) {
        self.log(LogLine::Offer(offer.clone()));
        self.tempo_pair = match offer.change {
            Some((Axis::Tempo, old, new)) => Some((old, new)),
            _ => None,
        };
        self.offer = Some(offer);
        self.offer_ctx = Some(ctx);
        self.offer_rev = self.offer_rev.wrapping_add(1);
    }

    /// The plan behind the play button.
    pub fn plan(&self) -> Option<&BlockPlan> {
        let o = self.offer.as_ref()?;
        o.cands.get(o.chosen).map(|c| &c.plan)
    }

    /// The one change of the offer (a pictogram, never text): tempo or rhythm, upward only.
    pub fn change(&self) -> Option<(Axis, u16, u16)> {
        self.offer.as_ref()?.change
    }

    /// What the screen is set to before any chip: the plan, with an offered tempo step left
    /// at the old tempo (the chip takes it).
    pub fn base_spec(&self) -> Option<BlockSpec> {
        let p = self.plan()?;
        Some(match self.tempo_pair {
            Some((old, _)) => BlockSpec { bpm: old, ..p.spec },
            None => p.spec,
        })
    }

    /// The player asked for something else (a skipped probe costs nothing). `false` when
    /// there is no offer.
    pub fn something_else(&mut self) -> bool {
        let Some(prev) = self.offer.clone() else {
            return false;
        };
        let ctx = self.offer_ctx.unwrap_or_else(|| self.ctx(self.now_ms()));
        let choice = choice_for_else(&prev);
        let next = match something_else_in(&prev, &self.ledger, &ctx, &self.cfg) {
            Some(o) => o,
            // Stale (the books moved on): start from a fresh offer.
            None => {
                let c = self.ctx(self.now_ms());
                self.offer_no += 1;
                next_offer(&self.ledger, &c, &self.cfg)
            }
        };
        if choice == Choice::ProbeSkipped {
            self.probe_done = true;
        }
        self.log(LogLine::Choice(choice));
        let ctx = self.ctx(self.now_ms());
        self.set_offer(next, ctx);
        true
    }

    // -------------------------------------------------------------- the block

    /// Play was pressed with `spec` on the screen. Takes the offer (it is answered), notes the
    /// block, and tells whether its summary is shown.
    pub fn begin_block(&mut self, spec: BlockSpec) {
        let now = self.now_ms();
        let plan = self.plan().cloned();
        let (kind, show) = match &plan {
            Some(p) if p.spec == spec => (p.kind, p.show_summary),
            // The player changed the tempo (the chip): an honest "free" block.
            Some(_) => (BlockKind::Free, true),
            None => (BlockKind::Free, true),
        };
        if plan.is_some() {
            self.log(LogLine::Choice(Choice::Accepted));
        }
        if kind == BlockKind::Probe {
            self.probe_done = true;
        }
        let after_gap = self.after_gap(now);
        self.active = Some(ActiveBlock {
            kind,
            spec,
            after_gap,
            ts_ms: now,
        });
        self.show_summary = show;
        self.offer = None;
        self.offer_ctx = None;
        self.tempo_pair = None;
        self.outs.clear();
        self.finished_ts = None;
    }

    fn after_gap(&self, now: i64) -> bool {
        let gap = (self.cfg.probe_gap_hours * 3_600_000.0) as i64;
        self.ledger
            .last_block_end_ms
            .is_some_and(|end| now - end >= gap)
    }

    fn on_round_start(&mut self, chart: &Chart) {
        self.chart = Some(chart.clone());
        self.outs.clear();
        self.finished_ts = None;
        if self.active.is_none() {
            // A round started without the Pulse flow: record it as a free block.
            let now = self.now_ms();
            self.show_summary = true;
            self.active = Some(ActiveBlock {
                kind: BlockKind::Free,
                spec: BlockSpec {
                    exercise: exercise_of(chart),
                    bpm: chart.bpm_at(0).round().clamp(20.0, 240.0) as u16,
                    bars: bars_of(chart),
                },
                after_gap: self.after_gap(now),
                ts_ms: now,
            });
        }
    }

    fn on_judge(&mut self, out: &JudgeOut) {
        self.outs.push(*out);
        // A contest that arrives after the block was written is a log line; the books are
        // folded again (the same as if the event had never existed).
        if let JudgeOut::Withdrawn {
            id,
            why: Why::Disputed,
        } = *out
            && let Some(ts) = self.finished_ts
        {
            self.log(LogLine::Dispute { block_ts: ts, id });
            self.display.on_dispute(ts, id);
            let lines = self.store.lines().to_vec();
            self.ledger = LearnerLedger::fold(&lines, &self.cfg);
        }
    }

    /// The round ended: write the block, fold the books, make the next offer. Returns the
    /// record (none when the round left no evidence).
    fn on_round_done(&mut self) -> Option<BlockRecord> {
        let active = self.active.take()?;
        let chart = self.chart.take()?;
        let (evidence, strums) = evidence_of_round(&chart, &self.outs, &self.gates);
        self.outs.clear();
        self.offer = None;
        let record = if evidence.is_empty() {
            None
        } else {
            let end = self.now_ms();
            let rec = BlockRecord {
                ts_ms: active.ts_ms,
                dur_ms: (end - active.ts_ms).clamp(0, i64::from(u32::MAX)) as u32,
                session: self.session,
                kind: active.kind,
                spec: active.spec,
                after_gap: active.after_gap,
                strums,
                evidence,
                prov: Provenance {
                    judge_v: "pulse-1".into(),
                    basis: self.basis.clone(),
                    gate: self.gates.state(VerdictType::Timing),
                    tape: None,
                },
            };
            let grades = self.ledger.apply(&rec, &self.cfg);
            self.display.apply(&rec, &grades);
            self.log(LogLine::Block(rec.clone()));
            self.save_display();
            self.blocks_this_session += 1;
            self.finished_ts = Some(rec.ts_ms);
            Some(rec)
        };
        self.ensure_offer();
        record
    }
}

// ------------------------------------------------------------------ messages

/// A finished block, ready to fold (sent after the books took it).
#[derive(Message, Clone, Debug)]
pub struct BlockFinished(pub BlockRecord);

/// The player answered the offer (key `O`: something else, probe skipped).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferAnswered(pub Choice);

/// The offer changed; views show one pictogram (tempo or rhythm), no text.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferChanged;

// ------------------------------------------------------------------ systems

/// Round start, judge outputs and round end, from the session's notes.
fn collect_round(
    mut learner: ResMut<Learner>,
    mut cmds: MessageReader<AudioCommand>,
    mut notes: MessageReader<AudioNote>,
    mut finished: MessageWriter<BlockFinished>,
    mut changed: MessageWriter<OfferChanged>,
) {
    for c in cmds.read() {
        if let AudioCommand::StartRound { chart, .. } = c {
            learner.on_round_start(chart);
        }
    }
    for AudioNote(ev) in notes.read() {
        match ev {
            SessionEvent::Judge(out) => learner.on_judge(out),
            SessionEvent::RoundDone { .. } => {
                let rev = learner.offer_rev;
                if let Some(rec) = learner.on_round_done() {
                    finished.write(BlockFinished(rec));
                }
                if learner.offer_rev != rev {
                    changed.write(OfferChanged);
                }
            }
            _ => {}
        }
    }
}

/// Key `O` asks for something else. The flow applies the answer.
fn read_else_key(keys: Option<Res<ButtonInput<KeyCode>>>, mut out: MessageWriter<OfferAnswered>) {
    if keys.is_some_and(|k| k.just_pressed(ELSE_KEY)) {
        out.write(OfferAnswered(Choice::SomethingElse));
    }
}

pub struct LearnLogicPlugin;

impl Plugin for LearnLogicPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Learner::guest())
            .add_message::<AudioCommand>()
            .add_message::<AudioNote>()
            .add_message::<BlockFinished>()
            .add_message::<OfferAnswered>()
            .add_message::<OfferChanged>()
            .add_systems(
                Update,
                (read_else_key, collect_round).chain().in_set(LearnSet),
            );
    }
}

/// The stored profile under `data`: the first folder of `profiles/`, or a new one.
fn stored_learner(data: &Path) -> io::Result<Learner> {
    let existing = std::fs::read_dir(data.join("profiles"))
        .ok()
        .and_then(|rd| {
            let mut names: Vec<String> = rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            names.sort();
            names.into_iter().next()
        });
    let id = match existing {
        Some(n) => ProfileId::Local(n),
        None => create_local(data, wall_ms() as u64, wall_ms())?,
    };
    Learner::open_local(data, &id)
}

pub fn install(app: &mut App, cfg: &AppConfig) {
    app.add_plugins(LearnLogicPlugin);
    // `--data <dir>` keeps the profile on disk (read once, here); the default is Guest.
    if let Some(dir) = &cfg.data {
        match stored_learner(dir) {
            Ok(l) => {
                app.insert_resource(l);
            }
            Err(e) => eprintln!("--data: profile not opened ({e}); using the Guest profile"),
        }
    }
}
