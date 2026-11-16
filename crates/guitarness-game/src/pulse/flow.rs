//! Pulse flow: home, count-in, round, between rounds. No window needed.
//!
//! There is no failure state: a round that went badly ends like any other, the counters only
//! grow, and the tempo changes only when the player accepts an offer (a chip).
//! Backspace and `AudioCommand::Dispute` belong to the summary module (G4), not to this one.

use bevy::prelude::*;
use guitarness_core::chart::{Chart as CoreChart, Dir, EventKind};
use guitarness_core::pulse::{Exercise, exercise, expectations};
use guitarness_core::stats::RoundStats;
use guitarness_core::timing::Level;
use guitarness_learn::types::{Axis, BlockSpec, Choice};
use guitarness_session::session::SessionEvent;

use super::assets::PulseView;
use super::feedback::PulseFeedback;
use crate::audio::{AudioCommand, AudioLink, AudioNote, VideoOffset};
use crate::gfx::SongClock;
use crate::learn::{LearnSet, Learner, OfferAnswered, OfferChanged};

pub const BPM_STEP: f64 = 5.0;
pub const BPM_MIN: f64 = 40.0;
pub const BPM_MAX: f64 = 160.0;
pub const COUNT_IN_BEATS: u32 = 4;

/// Where the player is. Every state is a calm one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    /// One button: play.
    #[default]
    Home,
    /// The count-in clicks (chart time below zero).
    Counting,
    Playing,
    /// A round ended (or stopped); play again, or take an offer.
    Between,
}

impl Phase {
    pub const ALL: [Phase; 4] = [Phase::Home, Phase::Counting, Phase::Playing, Phase::Between];
}

/// What the player can accept in a chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    TempoUp,
    TempoDown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Offers {
    pub up: bool,
    pub down: bool,
}

#[derive(Resource, Clone, Debug)]
pub struct PulseFlow {
    /// The Pulse screen is the one showing (the gallery cameras rest meanwhile).
    pub screen_on: bool,
    pub phase: Phase,
    pub view: PulseView,
    pub bpm: f64,
    pub exercise: Exercise,
    pub level: Level,
    /// Bars of the pattern (2 to 4) repeated until `target_s`.
    pub pattern_bars: u32,
    /// Length of a round, seconds (30 to 60).
    pub target_s: f64,
    pub offers: Offers,
    /// Rounds started in this session of the screen.
    pub round_no: u32,
}

impl Default for PulseFlow {
    fn default() -> Self {
        PulseFlow {
            screen_on: false,
            phase: Phase::Home,
            view: PulseView::D3,
            bpm: 80.0,
            exercise: Exercise::OnClickQuarters,
            level: Level::Apprentice,
            pattern_bars: 2,
            target_s: 40.0,
            offers: Offers::default(),
            round_no: 0,
        }
    }
}

/// Counters that only go up.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PulseTotals {
    strikes: u64,
    rounds: u64,
    heard: u64,
}

impl PulseTotals {
    pub fn strikes(&self) -> u64 {
        self.strikes
    }
    pub fn rounds(&self) -> u64 {
        self.rounds
    }
    pub fn heard(&self) -> u64 {
        self.heard
    }
}

/// The round on screen: what the views spawn from.
#[derive(Resource, Clone, Debug, Default)]
pub struct PulseRound {
    /// Bumped at every new round; the views respawn on change.
    pub rev: u32,
    pub bpm: f64,
    pub beat_s: f32,
    /// Seconds after time zero of each expected stroke.
    pub times: Vec<f32>,
    pub dirs: Vec<Dir>,
    /// Downbeats (beat 1 of a bar): drawn bigger, with a second ring.
    pub strong: Vec<bool>,
    pub sizes: Vec<f32>,
    pub length_s: f32,
}

impl PulseRound {
    /// Scale of the filled arrow of stroke `id`.
    pub fn fill_scale(&self, id: usize) -> f32 {
        self.sizes.get(id).copied().unwrap_or(1.0) * 0.46
    }

    /// Index of the first stroke not yet behind `t`.
    pub fn next_after(&self, t: f32) -> Option<usize> {
        self.times.iter().position(|x| *x >= t - 0.12)
    }
}

/// What the player asked for (keys, clicks), or the shots driver.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PulseAction {
    Play,
    ToggleView,
    Accept(Offer),
    /// Leave the round (Esc).
    Stop,
    /// Screen latency in steps of 5 ms (keys `[` and `]`).
    VideoNudge(i8),
}

/// The chart handed to the session for the current settings.
pub fn build_chart(flow: &PulseFlow) -> CoreChart {
    build_chart_bars(flow, None)
}

/// Like [`build_chart`]; `Some(bars)` is the exact length the learner's plan asks for.
pub fn build_chart_bars(flow: &PulseFlow, bars: Option<u32>) -> CoreChart {
    if let Some(b) = bars {
        return exercise(flow.exercise, flow.bpm, b.max(1));
    }
    let beat = 60.0 / flow.bpm;
    let bar = 4.0 * beat;
    let pat = flow.pattern_bars.clamp(2, 4);
    let mut bars = (flow.target_s.clamp(1.0, 60.0) / bar).ceil() as u32;
    bars = bars.div_ceil(pat) * pat;
    exercise(flow.exercise, flow.bpm, bars.max(pat))
}

pub fn build_round(chart: &CoreChart) -> PulseRound {
    let ex = expectations(chart);
    let beat_s = (60.0 / chart.bpm_at(0)) as f32;
    let evs: Vec<_> = chart
        .events()
        .iter()
        .filter(|e| !matches!(e.kind, EventKind::Rest))
        .collect();
    let small = ex.len() > 1 && ex[1].t_s - ex[0].t_s < f64::from(beat_s) * 0.75;
    let mut r = PulseRound {
        bpm: chart.bpm_at(0),
        beat_s,
        ..Default::default()
    };
    for (k, e) in ex.iter().enumerate() {
        let dir = match evs.get(k).map(|e| &e.kind) {
            Some(EventKind::Strum { dir, .. } | EventKind::Mute { dir }) => *dir,
            _ => Dir::Down,
        };
        let strong = e.pos_in_bar == 0;
        r.times.push(e.t_s as f32);
        r.dirs.push(dir);
        r.strong.push(strong);
        r.sizes
            .push(super::assets::ring_size(strong, small && !strong));
    }
    r.length_s = r.times.last().copied().unwrap_or(0.0) + beat_s;
    r
}

/// Tempo offers after a round. Only ever offers; nothing changes by itself.
pub fn offers_for(stats: &RoundStats, bpm: f64) -> Offers {
    if stats.events < 8 {
        return Offers::default();
    }
    let heard = stats.heard_share().unwrap_or(0.0);
    let in_win = stats.in_window_share().unwrap_or(0.0);
    Offers {
        up: heard >= 0.9 && in_win >= 0.7 && bpm + BPM_STEP <= BPM_MAX,
        down: heard < 0.6 && bpm - BPM_STEP >= BPM_MIN,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn handle_actions(
    mut acts: MessageReader<PulseAction>,
    mut flow: ResMut<PulseFlow>,
    mut round: ResMut<PulseRound>,
    mut fb: ResMut<PulseFeedback>,
    mut cmds: MessageWriter<AudioCommand>,
    video: Option<ResMut<VideoOffset>>,
    learner: Option<ResMut<Learner>>,
) {
    let mut video = video;
    let mut learner = learner;
    for a in acts.read() {
        match *a {
            PulseAction::Play => {
                if matches!(flow.phase, Phase::Home | Phase::Between) {
                    let chart = match learner.as_deref_mut() {
                        Some(l) => {
                            // The offer behind the button is on the screen; play that.
                            sync_offer(&mut flow, l);
                            let chart = build_chart_bars(&flow, Some(u32::from(l.bars)));
                            l.begin_block(spec_of(&flow, l.bars));
                            chart
                        }
                        None => build_chart(&flow),
                    };
                    let next = build_round(&chart);
                    fb.reset(next.times.len(), flow.level.window_ms() as f32);
                    let rev = round.rev + 1;
                    *round = next;
                    round.rev = rev;
                    flow.offers = Offers::default();
                    flow.round_no += 1;
                    flow.phase = Phase::Counting;
                    cmds.write(AudioCommand::StartRound {
                        chart,
                        lead_in_beats: COUNT_IN_BEATS,
                        take: None,
                    });
                }
            }
            PulseAction::ToggleView => flow.view = flow.view.next(),
            PulseAction::Accept(o) => {
                // Only an offer that is on the table changes the tempo; accepting leaves the
                // opposite chip, so the change can be undone.
                let pair = learner.as_deref().and_then(|l| l.tempo_pair);
                match o {
                    Offer::TempoUp if flow.offers.up && pair.is_some() => {
                        // The learner's own step (up only); the chip left is the way back.
                        flow.bpm = f64::from(pair.map_or(0, |p| p.1));
                        flow.offers = Offers {
                            up: false,
                            down: true,
                        };
                    }
                    Offer::TempoDown if flow.offers.down && pair.is_some() => {
                        flow.bpm = f64::from(pair.map_or(0, |p| p.0));
                        flow.offers = Offers {
                            up: true,
                            down: false,
                        };
                    }
                    Offer::TempoUp if flow.offers.up => {
                        flow.bpm = (flow.bpm + BPM_STEP).min(BPM_MAX);
                        flow.offers = Offers {
                            up: false,
                            down: true,
                        };
                    }
                    Offer::TempoDown if flow.offers.down => {
                        flow.bpm = (flow.bpm - BPM_STEP).max(BPM_MIN);
                        flow.offers = Offers {
                            up: true,
                            down: false,
                        };
                    }
                    _ => {}
                }
            }
            PulseAction::Stop => {
                if matches!(flow.phase, Phase::Counting | Phase::Playing) {
                    cmds.write(AudioCommand::StopRound);
                    flow.phase = Phase::Between;
                }
            }
            PulseAction::VideoNudge(n) => {
                if let Some(v) = video.as_deref_mut() {
                    v.0 = (v.0 + f64::from(n) * 0.005).clamp(-0.25, 0.25);
                }
            }
        }
    }
}

pub fn on_notes(
    mut notes: MessageReader<AudioNote>,
    mut links: MessageReader<AudioLink>,
    clock: Res<SongClock>,
    mut flow: ResMut<PulseFlow>,
    mut fb: ResMut<PulseFeedback>,
    mut totals: ResMut<PulseTotals>,
    learner: Option<Res<Learner>>,
) {
    let learned = learner.is_some();
    for n in notes.read() {
        match &n.0 {
            SessionEvent::RoundArmed { .. } => {
                if matches!(flow.phase, Phase::Home | Phase::Between) {
                    flow.phase = Phase::Counting;
                }
            }
            SessionEvent::Judge(out) => {
                if fb.apply(*out, clock.t) {
                    totals.strikes += 1;
                }
            }
            SessionEvent::RoundDone {
                stats, interrupted, ..
            } => {
                totals.rounds += 1;
                totals.heard += u64::from(stats.heard);
                flow.phase = Phase::Between;
                flow.offers = if *interrupted || learned {
                    // With the learner the offer is its own (see `sync_offers`).
                    Offers::default()
                } else {
                    offers_for(stats, flow.bpm)
                };
            }
            SessionEvent::Paused(_) => flow.phase = Phase::Between,
            _ => {}
        }
    }
    for l in links.read() {
        if matches!(l, AudioLink::Lost) && matches!(flow.phase, Phase::Counting | Phase::Playing) {
            flow.phase = Phase::Between;
        }
    }
}

/// The tempo or rhythm of the block `bars` bars long on the screen.
fn spec_of(flow: &PulseFlow, bars: u8) -> BlockSpec {
    BlockSpec {
        exercise: flow.exercise,
        bpm: flow.bpm.round().clamp(1.0, 999.0) as u16,
        bars,
    }
}

/// Put the learner's offer on the screen: the block it plans, and its tempo step (if any) as
/// the one chip. There is a single offer surface; nothing else is added.
pub fn apply_offer(flow: &mut PulseFlow, l: &mut Learner) {
    if let Some(spec) = l.base_spec() {
        flow.exercise = spec.exercise;
        flow.bpm = f64::from(spec.bpm);
        l.bars = spec.bars;
    }
    let tempo = matches!(l.change(), Some((Axis::Tempo, _, _)));
    flow.offers = Offers {
        up: tempo,
        down: false,
    };
    l.applied_rev = l.offer_rev;
}

/// Make sure an offer exists and is on the screen.
fn sync_offer(flow: &mut PulseFlow, l: &mut Learner) {
    l.ensure_offer();
    if l.applied_rev != l.offer_rev {
        apply_offer(flow, l);
    }
}

/// Between rounds the screen follows the learner's offer.
pub fn sync_offers(mut flow: ResMut<PulseFlow>, learner: Option<ResMut<Learner>>) {
    let Some(mut l) = learner else { return };
    // A block that has not been folded yet (a stopped round waits for its end note) owns the
    // next offer: computing one now would log a second, stale offer line.
    if l.active.is_some() {
        return;
    }
    if matches!(flow.phase, Phase::Home | Phase::Between) {
        sync_offer(&mut flow, &mut l);
    }
}

/// "Something else" (key `O`): the next candidate of the same list; never a queue.
pub fn handle_answers(
    mut answers: MessageReader<OfferAnswered>,
    mut flow: ResMut<PulseFlow>,
    learner: Option<ResMut<Learner>>,
    mut changed: MessageWriter<OfferChanged>,
) {
    let mut learner = learner;
    for a in answers.read() {
        if a.0 == Choice::Accepted {
            continue;
        }
        if let Some(l) = learner.as_deref_mut()
            && matches!(flow.phase, Phase::Home | Phase::Between)
            && l.something_else()
        {
            apply_offer(&mut flow, l);
            changed.write(OfferChanged);
        }
    }
}

pub fn tick_phase(clock: Res<SongClock>, mut flow: ResMut<PulseFlow>) {
    if flow.phase == Phase::Counting && clock.t >= 0.0 {
        flow.phase = Phase::Playing;
    }
}

/// Logic only: no window, no render. Add the messages' owner (`AudioBridgePlugin`) first.
pub struct PulseLogicPlugin;

impl Plugin for PulseLogicPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PulseAction>()
            .add_message::<OfferAnswered>()
            .add_message::<OfferChanged>()
            .init_resource::<PulseFlow>()
            .init_resource::<PulseRound>()
            .init_resource::<PulseFeedback>()
            .init_resource::<PulseTotals>()
            .add_systems(
                Update,
                (
                    handle_actions,
                    handle_answers,
                    on_notes,
                    sync_offers,
                    tick_phase,
                )
                    .chain()
                    .after(LearnSet),
            );
    }
}
