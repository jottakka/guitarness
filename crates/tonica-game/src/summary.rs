//! G4: the end-of-round summary and the "that was wrong" dispute (plan G4, rules P5/P8).
//!
//! Two plugins, as the crate's convention asks: [`SummaryLogicPlugin`] (state from the session's
//! events, no rendering, tested with `MinimalPlugins`) and [`SummaryDrawPlugin`] (Bevy UI).
//!
//! What the player sees, in this order: what got better ([`Row::Improved`]); with nothing better,
//! the day's strum counter ([`Row::Day`], it only grows); then "firsts" and the personal best
//! (monotone too); then a tendency pictogram, only when the round claims one. Never an accuracy
//! percentage, an error count, milliseconds per strum or any number that goes down. The
//! milliseconds of the round (median, spread) live in the Details panel (key D, closed).
//!
//! Dispute (Backspace): [`AudioCommand::Dispute`] withdraws the last non-ghost *final* verdict
//! (core rule). The module mirrors the judge's output in a [`VerdictLog`], so it can say, at the
//! press, which event the press aims at ([`DisputeTarget`]) and, afterwards, whether it was
//! withdrawn ([`Dispute::Withdrawn`]) or nothing was ([`Dispute::NothingToWithdraw`], said
//! calmly). The mark of a contested event is a neutral "?" (never a hit, never an error).
//!
//! Keys (this module owns them): Backspace dispute, D details, Enter accepts the offered
//! narrower window. R (tape) is in `tapeui`. Colours: white, grey and pale blue; no red; shape
//! carries the meaning. No file is touched here.

use bevy::prelude::*;

use tonica_core::stats::{Improvement, RoundStats, Tendency, VerdictLog};
use tonica_core::timing::Level;
use tonica_core::verdict::{Attack, JudgeOut, PulseVerdict, Why};
use tonica_session::session::SessionEvent;

use crate::AppConfig;
use crate::audio::{AudioCommand, AudioNote};
use crate::learn::{LearnSet, Learner};

/// Columns of the histogram.
pub const BINS: usize = 9;
/// Half-width of the cloud and the histogram, ms (the widest window).
pub const CLOUD_SCALE_MS: f32 = 120.0;
/// A press that withdraws nothing within this time says so, seconds of real time.
pub const DISPUTE_WAIT_S: f64 = 0.6;
/// The dispute line stays on screen this long, seconds.
pub const DISPUTE_SHOWN_S: f64 = 4.0;
/// Heard strums needed for the "all heard" first.
const COMPLETE_MIN: u32 = 4;

// ------------------------------------------------------------------ model

/// One dot of the cloud: where in the bar, and how early (-1) or late (+1) against the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudDot {
    pub pos_in_bar: u8,
    pub x: f32,
}

/// A thing done for the first time (monotone: it can only be added).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum First {
    Round,
    Center,
    AllHeard,
}

/// Which way the round leans, with a shape and no number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trend {
    Early,
    Late,
}

/// A line of the summary, in screen order.
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub enum Row {
    Improved(Improvement),
    /// Strums heard today (only grows).
    Day(u32),
    First(First),
    /// A new personal best of strums on time in one round (only grows).
    Best(u32),
    Trend(Trend),
}

/// What the player did across rounds. Every field only grows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub rounds: u32,
    /// Strums heard, summed.
    pub strums: u32,
    /// Most strums on time (centre or in the window) in one round.
    pub best_on_time: u32,
    pub firsts: Vec<First>,
}

/// The finished round, as the summary keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct RoundSummary {
    pub stats: RoundStats,
    pub better: Vec<Improvement>,
    /// The offered narrower window (never taken without a press).
    pub suggest: Option<Level>,
    pub interrupted: bool,
    /// Statistics replaced after a dispute.
    pub revised: bool,
    pub cloud: Vec<CloudDot>,
    pub hist: [u16; BINS],
    pub new_firsts: Vec<First>,
    pub new_best: Option<u32>,
    /// `Totals::strums` after this round.
    pub strums_today: u32,
}

impl RoundSummary {
    /// The lines of the summary in the order the player must meet them.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self.better.iter().map(|i| Row::Improved(*i)).collect();
        if rows.is_empty() {
            rows.push(Row::Day(self.strums_today));
        }
        rows.extend(self.new_firsts.iter().map(|f| Row::First(*f)));
        if let Some(n) = self.new_best {
            rows.push(Row::Best(n));
        }
        if let Some(t) = self.trend() {
            rows.push(Row::Trend(t));
        }
        rows
    }

    /// Only a claimed tendency gives a trend; `Centered` and `NotClaimed` give none.
    pub fn trend(&self) -> Option<Trend> {
        match self.stats.tendency {
            Tendency::Early(_) => Some(Trend::Early),
            Tendency::Late(_) => Some(Trend::Late),
            Tendency::Centered | Tendency::NotClaimed(_) => None,
        }
    }
}

/// The event a dispute aims at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisputeTarget {
    pub id: u32,
    pub pos_in_bar: u8,
    /// What was shown for it: an attack heard or not (shape only).
    pub heard: bool,
}

/// Where the dispute is.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Dispute {
    #[default]
    Idle,
    /// Pressed: the "?" sits on `target` (`None`: nothing could be contested) until the session
    /// answers or [`DISPUTE_WAIT_S`] passes.
    Asked {
        target: Option<DisputeTarget>,
        at_s: f64,
    },
    /// The session withdrew it: out of the statistics, neither a hit nor an error.
    Withdrawn { target: DisputeTarget, at_s: f64 },
    /// Nothing was withdrawn (no final verdict yet, or it was already out).
    NothingToWithdraw { at_s: f64 },
}

/// The narrower-window offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Chip {
    #[default]
    None,
    Offered(Level),
    Taken(Level),
}

/// Everything the summary and the dispute line draw from.
#[derive(Resource, Debug, Default)]
pub struct SummaryState {
    pub summary: Option<RoundSummary>,
    pub visible: bool,
    pub details: bool,
    pub chip: Chip,
    pub totals: Totals,
    pub dispute: Dispute,
    /// Real seconds since the app started.
    pub now_s: f64,
    /// Grows at every change the drawing has to follow.
    pub rev: u32,
}

/// What the key presses mean (the keys system writes them; tests write them too).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryAction {
    Dispute,
    ToggleDetails,
    AcceptSuggestion,
}

/// The judge's output for the running (or last) round, as the player saw it.
#[derive(Resource, Default)]
struct Mirror {
    log: VerdictLog,
    /// Id of the last non-ghost final verdict shown.
    last_final: Option<u32>,
}

impl Mirror {
    fn clear(&mut self) {
        self.log.clear();
        self.last_final = None;
    }

    fn verdict(&self, id: u32) -> Option<&PulseVerdict> {
        self.log.verdicts().iter().rfind(|v| v.id == id)
    }

    fn target(&self, v: &PulseVerdict) -> DisputeTarget {
        DisputeTarget {
            id: v.id,
            pos_in_bar: v.pos_in_bar,
            heard: matches!(v.attack, Attack::Heard),
        }
    }

    /// The event a press would withdraw: the last non-ghost final verdict, if still in the
    /// statistics (judged; the same rule as the judge's).
    fn pending_target(&self) -> Option<DisputeTarget> {
        let v = self.verdict(self.last_final?)?;
        matches!(v.attack, Attack::Heard | Attack::NotHeard).then(|| self.target(v))
    }

    fn feed(&mut self, out: &JudgeOut) {
        if let JudgeOut::Verdict(v) = out
            && v.fin
            && !matches!(v.attack, Attack::NotJudged(Why::Ghost))
        {
            self.last_final = Some(v.id);
        }
        self.log.push(out);
    }

    fn cloud(&self) -> (Vec<CloudDot>, [u16; BINS]) {
        let mut dots = Vec::new();
        let mut hist = [0u16; BINS];
        for v in self.log.verdicts() {
            if let Some(dt) = v.dt_ms {
                let x = (dt / CLOUD_SCALE_MS).clamp(-1.0, 1.0);
                dots.push(CloudDot {
                    pos_in_bar: v.pos_in_bar,
                    x,
                });
                let bin = (((x + 1.0) * 0.5 * BINS as f32) as usize).min(BINS - 1);
                hist[bin] = hist[bin].saturating_add(1);
            }
        }
        (dots, hist)
    }
}

// ------------------------------------------------------------------ logic

fn update_totals(
    totals: &mut Totals,
    stats: &RoundStats,
    interrupted: bool,
) -> (Vec<First>, Option<u32>) {
    let first_round = totals.rounds == 0;
    totals.rounds += 1;
    totals.strums = totals.strums.saturating_add(stats.heard);
    if interrupted {
        return (Vec::new(), None);
    }
    let mut new_firsts = Vec::new();
    let mut add = |f: First, totals: &mut Totals| {
        if !totals.firsts.contains(&f) {
            totals.firsts.push(f);
            new_firsts.push(f);
        }
    };
    add(First::Round, totals);
    if stats.center > 0 {
        add(First::Center, totals);
    }
    if stats.not_heard == 0 && stats.heard >= COMPLETE_MIN {
        add(First::AllHeard, totals);
    }
    let on_time = stats.center + stats.in_window;
    let new_best =
        (!first_round && on_time > totals.best_on_time && on_time > 0).then_some(on_time);
    totals.best_on_time = totals.best_on_time.max(on_time);
    (new_firsts, new_best)
}

#[allow(clippy::too_many_arguments)]
fn take_notes(
    time: Res<Time<Real>>,
    mut notes: MessageReader<AudioNote>,
    mut st: ResMut<SummaryState>,
    mut mirror: ResMut<Mirror>,
    learner: Option<Res<Learner>>,
) {
    st.now_s = time.elapsed_secs_f64();
    for AudioNote(ev) in notes.read() {
        match ev {
            SessionEvent::RoundArmed { .. } => {
                mirror.clear();
                st.visible = false;
                st.chip = Chip::None;
                st.dispute = Dispute::Idle;
                st.rev = st.rev.wrapping_add(1);
            }
            SessionEvent::Judge(out) => {
                mirror.feed(out);
                if let JudgeOut::Withdrawn {
                    id,
                    why: Why::Disputed,
                } = *out
                {
                    let target = match st.dispute {
                        Dispute::Asked {
                            target: Some(t), ..
                        } if t.id == id => Some(t),
                        _ => mirror.verdict(id).map(|v| mirror.target(v)),
                    };
                    if let Some(target) = target {
                        st.dispute = Dispute::Withdrawn {
                            target,
                            at_s: st.now_s,
                        };
                        st.rev = st.rev.wrapping_add(1);
                    }
                }
            }
            SessionEvent::RoundDone {
                stats,
                better,
                suggest,
                interrupted,
            } => {
                // A probe block (the retention check) builds no summary at all: its result is
                // never shown, so there is no entity to draw and nothing to read.
                if learner.as_deref().is_some_and(|l| !l.show_summary) {
                    st.summary = None;
                    st.visible = false;
                    st.chip = Chip::None;
                    st.rev = st.rev.wrapping_add(1);
                    continue;
                }
                let (new_firsts, new_best) = update_totals(&mut st.totals, stats, *interrupted);
                let (cloud, hist) = mirror.cloud();
                // The counter reads the display book (only grows); without a learner, the
                // totals of this run.
                let strums_today = learner.as_deref().map_or(st.totals.strums, |l| {
                    l.display.strums.get().min(u64::from(u32::MAX)) as u32
                });
                st.chip = match suggest {
                    Some(l) if !*interrupted => Chip::Offered(*l),
                    _ => Chip::None,
                };
                st.summary = Some(RoundSummary {
                    stats: *stats,
                    better: if *interrupted {
                        Vec::new()
                    } else {
                        better.clone()
                    },
                    suggest: *suggest,
                    interrupted: *interrupted,
                    revised: false,
                    cloud,
                    hist,
                    new_firsts,
                    new_best,
                    strums_today,
                });
                st.visible = true;
                st.rev = st.rev.wrapping_add(1);
            }
            SessionEvent::RoundRevised { stats } => {
                let (cloud, hist) = mirror.cloud();
                if let Some(s) = st.summary.as_mut() {
                    s.stats = *stats;
                    s.cloud = cloud;
                    s.hist = hist;
                    s.revised = true;
                    st.rev = st.rev.wrapping_add(1);
                }
            }
            _ => {}
        }
    }
}

fn actions(
    mut acts: MessageReader<SummaryAction>,
    mut cmds: MessageWriter<AudioCommand>,
    mut st: ResMut<SummaryState>,
    mirror: Res<Mirror>,
) {
    for a in acts.read() {
        match a {
            SummaryAction::Dispute => {
                if matches!(st.dispute, Dispute::Asked { .. }) {
                    continue; // one press at a time: the answer is still on its way
                }
                st.dispute = Dispute::Asked {
                    target: mirror.pending_target(),
                    at_s: st.now_s,
                };
                st.rev = st.rev.wrapping_add(1);
                cmds.write(AudioCommand::Dispute);
            }
            SummaryAction::ToggleDetails => {
                st.details = !st.details;
                st.rev = st.rev.wrapping_add(1);
            }
            SummaryAction::AcceptSuggestion => {
                if let Chip::Offered(l) = st.chip {
                    st.chip = Chip::Taken(l);
                    st.rev = st.rev.wrapping_add(1);
                    cmds.write(AudioCommand::SetLevel(l));
                }
            }
        }
    }
}

/// A press the session did not answer by withdrawing something says so after a short wait.
fn dispute_clock(mut st: ResMut<SummaryState>) {
    if let Dispute::Asked { at_s, .. } = st.dispute
        && st.now_s - at_s >= DISPUTE_WAIT_S
    {
        st.dispute = Dispute::NothingToWithdraw { at_s: st.now_s };
        st.rev = st.rev.wrapping_add(1);
    }
}

fn keys(keys: Option<Res<ButtonInput<KeyCode>>>, mut out: MessageWriter<SummaryAction>) {
    let Some(k) = keys else { return };
    if k.just_pressed(KeyCode::Backspace) {
        out.write(SummaryAction::Dispute);
    }
    if k.just_pressed(KeyCode::KeyD) {
        out.write(SummaryAction::ToggleDetails);
    }
    if k.just_pressed(KeyCode::Enter) {
        out.write(SummaryAction::AcceptSuggestion);
    }
}

/// State and keys, no rendering.
pub struct SummaryLogicPlugin;

impl Plugin for SummaryLogicPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<AudioCommand>()
            .add_message::<AudioNote>()
            .add_message::<SummaryAction>()
            .init_resource::<SummaryState>()
            .init_resource::<Mirror>()
            .add_systems(
                Update,
                (keys, take_notes.after(LearnSet), actions, dispute_clock).chain(),
            );
    }
}

// ------------------------------------------------------------------ drawing

const PANEL: Color = Color::srgba(0.04, 0.05, 0.09, 0.92);
const INK: Color = Color::srgb(0.88, 0.92, 0.98);
const SOFT: Color = Color::srgb(0.58, 0.64, 0.74);
const ACCENT: Color = Color::srgb(0.55, 0.85, 1.0);

/// Root of the summary panel.
#[derive(Component)]
pub struct SummaryRoot;
/// The tendency pictogram (absent unless a tendency is claimed).
#[derive(Component)]
pub struct TrendMark(pub Trend);
/// The Details panel (only while open).
#[derive(Component)]
pub struct DetailsPanel;
/// The offered narrower window.
#[derive(Component)]
pub struct ChipMark(pub Level);
/// "Space: play again", the main next step.
#[derive(Component)]
pub struct PlayHint;

/// pt-BR, plain: what the key does and that it is optional.
pub const PLAY_HINT: &str = "Espaço: tocar de novo";
pub const TEMPO_UP_HINT: &str = "Cima (opcional): mais rápido";
pub const TEMPO_DOWN_HINT: &str = "Baixo (opcional): mais devagar";
pub const RHYTHM_HINT: &str = "Próximo: colcheias";
pub const ELSE_HINT: &str = "O (opcional): outra coisa";
pub const CHIP_HINT: &str = "Enter (opcional): deixar o tempo mais exigente";
/// Root of the dispute line.
#[derive(Component)]
pub struct DisputeLine;
/// The neutral "?" of a contested event.
#[derive(Component)]
pub struct QuestionMark;

fn first_text(f: First) -> &'static str {
    match f {
        First::Round => "Primeira rodada feita",
        First::Center => "Primeiro golpe no centro",
        First::AllHeard => "Primeira rodada com todos os golpes ouvidos",
    }
}

/// The line's text, pt-BR.
pub fn row_text(r: &Row) -> String {
    match r {
        Row::Improved(Improvement::MoreHeard { .. }) => "Mais golpes ouvidos".into(),
        Row::Improved(Improvement::MoreInWindow { .. }) => "Mais golpes no tempo".into(),
        Row::Improved(Improvement::Steadier { .. }) => "Mais constante".into(),
        Row::Improved(Improvement::CloserToCenter { .. }) => "Mais perto do clique".into(),
        Row::Day(n) => format!("Golpes no total: {n}"),
        Row::First(f) => first_text(*f).into(),
        Row::Best(n) => format!("Sua melhor rodada: {n} golpes no tempo"),
        Row::Trend(Trend::Early) => "Você chega um pouco antes do clique".into(),
        Row::Trend(Trend::Late) => "Você chega um pouco depois do clique".into(),
    }
}

/// The dispute line's text, pt-BR.
pub fn dispute_text(d: &Dispute) -> Option<String> {
    match d {
        Dispute::Idle => None,
        Dispute::Asked {
            target: Some(t), ..
        } => Some(format!(
            "Contestando o golpe {} do compasso",
            t.pos_in_bar + 1
        )),
        Dispute::Asked { target: None, .. } => Some("Contestando...".into()),
        Dispute::Withdrawn { target, .. } => Some(format!(
            "Ok, o golpe {} do compasso saiu da conta",
            target.pos_in_bar + 1
        )),
        Dispute::NothingToWithdraw { .. } => Some("Não havia nada para tirar agora".into()),
    }
}

fn text(parent: &mut ChildSpawnerCommands, s: impl Into<String>, size: f32, color: Color) {
    parent.spawn((
        Text::new(s.into()),
        TextFont {
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(color),
    ));
}

fn bar(parent: &mut ChildSpawnerCommands, w: f32, h: f32, color: Color) {
    parent.spawn((
        Node {
            width: Val::Px(w),
            height: Val::Px(h),
            ..default()
        },
        BackgroundColor(color),
    ));
}

/// An outline circle of `d` px.
fn ring(parent: &mut ChildSpawnerCommands, d: f32, color: Color) {
    parent.spawn((
        Node {
            width: Val::Px(d),
            height: Val::Px(d),
            border: UiRect::all(Val::Px(3.0)),
            border_radius: BorderRadius::MAX,
            ..default()
        },
        BorderColor::all(color),
    ));
}

/// A filled circle of `d` px.
fn disc(parent: &mut ChildSpawnerCommands, d: f32, color: Color) {
    parent.spawn((
        Node {
            width: Val::Px(d),
            height: Val::Px(d),
            border_radius: BorderRadius::MAX,
            ..default()
        },
        BackgroundColor(color),
    ));
}

/// Pictogram of a row: shapes only, one look per kind of row.
fn picto(parent: &mut ChildSpawnerCommands, r: &Row) {
    parent
        .spawn(Node {
            width: Val::Px(30.0),
            height: Val::Px(22.0),
            align_items: AlignItems::FlexEnd,
            justify_content: JustifyContent::Center,
            column_gap: Val::Px(3.0),
            ..default()
        })
        .with_children(|p| match r {
            // rising bars
            Row::Improved(_) => {
                bar(p, 6.0, 8.0, ACCENT);
                bar(p, 6.0, 14.0, ACCENT);
                bar(p, 6.0, 20.0, ACCENT);
            }
            // a plain row of equal bars: a count that only grows
            Row::Day(_) => {
                bar(p, 6.0, 14.0, INK);
                bar(p, 6.0, 14.0, INK);
                bar(p, 6.0, 14.0, INK);
            }
            // a podium: the tallest bar in the middle
            Row::Best(_) => {
                bar(p, 6.0, 10.0, INK);
                bar(p, 6.0, 20.0, INK);
                bar(p, 6.0, 14.0, INK);
            }
            // a finished lap: an empty ring
            Row::First(First::Round) => ring(p, 20.0, ACCENT),
            // a bullseye: a ring with a dot in it
            Row::First(First::Center) => {
                p.spawn((
                    Node {
                        width: Val::Px(20.0),
                        height: Val::Px(20.0),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        border: UiRect::all(Val::Px(3.0)),
                        border_radius: BorderRadius::MAX,
                        ..default()
                    },
                    BorderColor::all(ACCENT),
                ))
                .with_children(|c| disc(c, 6.0, ACCENT));
            }
            // three dots: every stroke heard
            Row::First(First::AllHeard) => {
                disc(p, 8.0, ACCENT);
                disc(p, 8.0, ACCENT);
                disc(p, 8.0, ACCENT);
            }
            // a block that leans to one side of a centre tick
            Row::Trend(Trend::Early) => {
                bar(p, 12.0, 6.0, INK);
                bar(p, 3.0, 20.0, SOFT);
            }
            Row::Trend(Trend::Late) => {
                bar(p, 3.0, 20.0, SOFT);
                bar(p, 12.0, 6.0, INK);
            }
        });
}

fn cloud(parent: &mut ChildSpawnerCommands, s: &RoundSummary) {
    parent
        .spawn(Node {
            width: Val::Px(300.0),
            height: Val::Px(70.0),
            position_type: PositionType::Relative,
            ..default()
        })
        .with_children(|p| {
            // centre tick: the click
            p.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(149.0),
                    width: Val::Px(2.0),
                    height: Val::Px(70.0),
                    ..default()
                },
                BackgroundColor(SOFT),
            ));
            for d in &s.cloud {
                p.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px((d.x + 1.0) * 0.5 * 288.0),
                        top: Val::Px((d.pos_in_bar % 8) as f32 * 8.0),
                        width: Val::Px(8.0),
                        height: Val::Px(8.0),
                        border_radius: BorderRadius::MAX,
                        ..default()
                    },
                    BackgroundColor(INK.with_alpha(0.45)),
                ));
            }
        });
    let top = s.hist.iter().copied().max().unwrap_or(0).max(1) as f32;
    parent
        .spawn(Node {
            height: Val::Px(30.0),
            align_items: AlignItems::FlexEnd,
            column_gap: Val::Px(2.0),
            ..default()
        })
        .with_children(|p| {
            for c in s.hist {
                bar(p, 30.0, (c as f32 / top * 28.0).max(2.0), SOFT);
            }
        });
    // Which side is which (words, no numbers).
    parent
        .spawn(Node {
            width: Val::Px(300.0),
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        })
        .with_children(|p| {
            text(p, "cedo", 13.0, SOFT);
            text(p, "no clique", 13.0, SOFT);
            text(p, "tarde", 13.0, SOFT);
        });
}

fn details(parent: &mut ChildSpawnerCommands, s: &RoundSummary) {
    let ms = |v: Option<f64>| v.map_or("-".to_string(), |x| format!("{x:.0} ms"));
    parent
        .spawn((
            DetailsPanel,
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                ..default()
            },
        ))
        .with_children(|p| {
            text(p, format!("Mediana: {}", ms(s.stats.median_ms)), 17.0, INK);
            text(
                p,
                format!("Dispersão: {}", ms(s.stats.spread_ms)),
                17.0,
                INK,
            );
            text(p, format!("Golpes ouvidos: {}", s.stats.heard), 17.0, INK);
            text(p, format!("Contestados: {}", s.stats.disputed), 17.0, INK);
        });
}

/// Last drawn: revision, tempo chips, rhythm offer, else-hint.
type DrawKey = (u32, bool, bool, bool, bool);

fn draw_summary(
    mut commands: Commands,
    st: Res<SummaryState>,
    flow: Option<Res<crate::pulse::flow::PulseFlow>>,
    old: Query<Entity, With<SummaryRoot>>,
    learner: Option<Res<Learner>>,
    mut seen: Local<Option<DrawKey>>,
) {
    let (tempo_up, tempo_down) = flow
        .as_ref()
        .map_or((false, false), |f| (f.offers.up, f.offers.down));
    let new_rhythm = learner.as_deref().is_some_and(|l| {
        matches!(
            l.change(),
            Some((guitarness_learn::types::Axis::Rhythm, _, _))
        )
    });
    let can_else = learner
        .as_deref()
        .is_some_and(|l| l.offer.as_ref().is_some_and(|o| o.cands.len() >= 2));
    let key = (st.rev, tempo_up, tempo_down, new_rhythm, can_else);
    if *seen == Some(key) {
        return;
    }
    *seen = Some(key);
    for e in &old {
        commands.entity(e).despawn();
    }
    let (true, Some(s)) = (st.visible, st.summary.as_ref()) else {
        return;
    };
    commands
        .spawn((
            SummaryRoot,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(24.0),
                top: Val::Px(24.0),
                width: Val::Px(340.0),
                padding: UiRect::all(Val::Px(16.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                border_radius: BorderRadius::all(Val::Px(12.0)),
                ..default()
            },
            BackgroundColor(PANEL),
        ))
        .with_children(|p| {
            if s.interrupted {
                text(p, "Rodada parada. Quando quiser, de novo.", 16.0, SOFT);
            }
            cloud(p, s);
            for r in s.rows() {
                let mut line = p.spawn((
                    r,
                    Node {
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(10.0),
                        ..default()
                    },
                ));
                if let Row::Trend(t) = r {
                    line.insert(TrendMark(t));
                }
                line.with_children(|l| {
                    picto(l, &r);
                    text(l, row_text(&r), 18.0, INK);
                });
            }
            // The next step: play again (Space). Always the first and clearest.
            p.spawn((
                PlayHint,
                Node {
                    padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(16.0)),
                    ..default()
                },
                BorderColor::all(INK),
            ))
            .with_children(|c| text(c, PLAY_HINT, 18.0, INK));
            if let Chip::Offered(level) = st.chip {
                p.spawn((
                    ChipMark(level),
                    Node {
                        padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                        border: UiRect::all(Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(14.0)),
                        ..default()
                    },
                    BorderColor::all(ACCENT),
                ))
                .with_children(|c| text(c, CHIP_HINT, 15.0, ACCENT));
            }
            // The tempo offers live here too, so nothing else competes on the lane.
            for (on, label) in [(tempo_up, TEMPO_UP_HINT), (tempo_down, TEMPO_DOWN_HINT)] {
                if on {
                    p.spawn((
                        Node {
                            padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(14.0)),
                            ..default()
                        },
                        BorderColor::all(ACCENT),
                    ))
                    .with_children(|c| text(c, label, 15.0, ACCENT));
                }
            }
            // The learner's one offer: a new rhythm is played next (pictogram: eighths), and
            // key O asks for something else. Both are optional and carry no number.
            if new_rhythm {
                p.spawn(Node {
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(10.0),
                    ..default()
                })
                .with_children(|l| {
                    l.spawn(Node {
                        width: Val::Px(30.0),
                        height: Val::Px(22.0),
                        align_items: AlignItems::FlexEnd,
                        justify_content: JustifyContent::Center,
                        column_gap: Val::Px(3.0),
                        ..default()
                    })
                    .with_children(|b| {
                        for h in [18.0, 10.0, 18.0, 10.0] {
                            bar(b, 4.0, h, ACCENT);
                        }
                    });
                    text(l, RHYTHM_HINT, 15.0, ACCENT);
                });
            }
            if can_else {
                text(p, ELSE_HINT, 15.0, SOFT);
            }
            if let Chip::Taken(_) = st.chip {
                text(p, "Janela de tempo mais exigente ligada", 15.0, SOFT);
            }
            if st.details {
                details(p, s);
            }
            text(
                p,
                if st.details {
                    "D: fechar"
                } else {
                    "D: detalhes"
                },
                16.0,
                INK,
            );
        });
}

fn draw_dispute(
    mut commands: Commands,
    st: Res<SummaryState>,
    old: Query<Entity, With<DisputeLine>>,
    mut seen: Local<Option<(u32, bool)>>,
) {
    let shown = match st.dispute {
        Dispute::Idle => false,
        Dispute::Asked { .. } => true,
        Dispute::Withdrawn { at_s, .. } | Dispute::NothingToWithdraw { at_s } => {
            st.now_s - at_s < DISPUTE_SHOWN_S
        }
    };
    if *seen == Some((st.rev, shown)) {
        return;
    }
    *seen = Some((st.rev, shown));
    for e in &old {
        commands.entity(e).despawn();
    }
    let (true, Some(line)) = (shown, dispute_text(&st.dispute)) else {
        return;
    };
    let marked = matches!(
        st.dispute,
        Dispute::Asked {
            target: Some(_),
            ..
        } | Dispute::Withdrawn { .. }
    );
    commands
        .spawn((
            DisputeLine,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(32.0),
                bottom: Val::Px(32.0),
                padding: UiRect::axes(Val::Px(18.0), Val::Px(10.0)),
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
                border_radius: BorderRadius::all(Val::Px(14.0)),
                ..default()
            },
            BackgroundColor(PANEL),
        ))
        .with_children(|p| {
            if marked {
                // A neutral dot: contested, neither a hit nor an error.
                p.spawn((
                    QuestionMark,
                    Node {
                        width: Val::Px(12.0),
                        height: Val::Px(12.0),
                        border_radius: BorderRadius::MAX,
                        ..default()
                    },
                    BackgroundColor(INK),
                ));
            }
            text(p, line, 19.0, INK);
        });
}

/// Panel and dispute line, in Bevy UI.
pub struct SummaryDrawPlugin;

impl Plugin for SummaryDrawPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (draw_summary, draw_dispute).after(dispute_clock));
    }
}

/// Called once by `build_app` after the base plugins and resources are in place.
pub fn install(app: &mut App, _cfg: &AppConfig) {
    app.add_plugins((SummaryLogicPlugin, SummaryDrawPlugin));
}
