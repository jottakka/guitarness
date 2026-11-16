//! The soundcheck on screen (unit G2).
//!
//! Two plugins, as the crate convention asks:
//!
//! - [`SoundcheckPlugin`] (logic, no rendering): the [`Soundcheck`] resource follows the
//!   calibration through [`AudioNote`] messages and answers [`SoundcheckCmd`] messages. It
//!   sends [`AudioCommand::Soundcheck`] to the bridge and tells the game with
//!   [`SoundcheckReady`] when a round may start.
//! - [`SoundcheckDrawPlugin`]: one pictogram per [`Prompt`], progress dots, the play-now
//!   button. Pictograms only: no text unless [`SoundcheckText`] is switched on (maintainer
//!   decision).
//!
//! Player rules kept here: "play now" is always there (without a calibration the rhythm is
//! shown and not judged); no outcome is a failure screen; a blocked microphone, an unstable
//! clock or a lost device end in a calm "try again / play now"; problems use white or grey
//! and a shape, never red; the progress dots only ever fill.
//!
//! `PlayNow` hides the screen and announces [`SoundcheckReady`]; a running calibration is
//! stopped with `AudioCommand::CancelSoundcheck` (session `CancelSoundcheck`, answered by
//! `SoundcheckCancelled`, handled here; the previous basis is kept).

use bevy::prelude::*;

use guitarness_audio::calib::{Basis, Prompt};
use guitarness_session::session::{SessionEvent, SoundOutcome};

use crate::AppConfig;
use crate::audio::{AudioCommand, AudioLink, AudioMode, AudioNote, AudioSet, AudioStatus, Link};
use crate::palette;

/// Dots in the progress row.
pub const DOTS: u8 = 4;

// ------------------------------------------------------------------ state

/// Where the soundcheck stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    /// Not started.
    #[default]
    Idle,
    /// The calibration runs.
    Running,
    /// It ended, whatever the outcome (never a failure).
    Finished,
}

/// What the pictogram shows. One per [`Prompt`], plus the device plug.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pictogram {
    /// Nothing asked yet.
    #[default]
    Waiting,
    /// Checking the saved setup with a few beeps.
    Checking,
    /// Stay quiet for a moment.
    Silence,
    /// Too loud: step back from the speaker.
    TooLoud,
    /// `count` beeps, nothing to play.
    Beeps { count: u8 },
    /// `count` more beeps, louder.
    BeepsLouder { count: u8 },
    /// Muted strum with the click; `count_in` clicks before, `beat` of `of` now.
    MutedStrum { count_in: u8, beat: u8, of: u8 },
    /// All set.
    Ready,
    /// Rhythm shown, judged later.
    RhythmLater,
    /// The microphone hears only silence.
    MicBlocked,
    /// The device went away: a plug.
    Plug,
}

/// One pictogram per prompt (exhaustive: a new `Prompt` fails the build here).
pub fn pictogram_for(p: Prompt) -> Pictogram {
    match p {
        Prompt::Checking => Pictogram::Checking,
        Prompt::Quiet => Pictogram::Silence,
        Prompt::TooLoud => Pictogram::TooLoud,
        Prompt::WillBeep { count } => Pictogram::Beeps { count },
        Prompt::WillBeepLouder { count } => Pictogram::BeepsLouder { count },
        Prompt::StrumMuted { count_in, beat, of } => Pictogram::MutedStrum { count_in, beat, of },
        Prompt::Done => Pictogram::Ready,
        Prompt::RhythmLater => Pictogram::RhythmLater,
        Prompt::MicBlocked => Pictogram::MicBlocked,
        Prompt::Interrupted => Pictogram::Plug,
    }
}

/// Progress stage of a prompt (0: does not move the dots).
fn stage_of(p: Prompt) -> u8 {
    match p {
        Prompt::Checking | Prompt::Quiet | Prompt::TooLoud => 1,
        Prompt::WillBeep { .. } | Prompt::WillBeepLouder { .. } => 2,
        Prompt::StrumMuted { .. } => 3,
        Prompt::Done => DOTS,
        Prompt::RhythmLater | Prompt::MicBlocked | Prompt::Interrupted => 0,
    }
}

/// What the player can do next. Playing is always one of the choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choices {
    /// Always true.
    pub play_now: bool,
    /// Offered when the soundcheck did not end with a verdict on rhythm.
    pub try_again: bool,
}

/// The soundcheck as the screen and the game see it.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Soundcheck {
    pub phase: Phase,
    /// The screen is up (hidden by "play now").
    pub shown: bool,
    pub prompt: Option<Prompt>,
    pub pictogram: Pictogram,
    /// Progress dots filled, 0..=[`DOTS`]. Only grows within one soundcheck.
    pub dots: u8,
    pub outcome: Option<SoundOutcome>,
    /// Basis in force (kept after the player chose "play now" too).
    pub basis: Option<Basis>,
    /// The player chose to play before the soundcheck ended.
    pub skipped: bool,
    /// Gentle hint: wireless headphones add delay; wired is steadier.
    pub bluetooth_hint: bool,
}

impl Default for Soundcheck {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            shown: false,
            prompt: None,
            pictogram: Pictogram::Waiting,
            dots: 0,
            outcome: None,
            basis: None,
            skipped: false,
            bluetooth_hint: false,
        }
    }
}

impl Soundcheck {
    /// "Play now" is always available.
    pub fn playable(&self) -> bool {
        true
    }

    /// Timing is judged only with a basis other than `None` (rhythm shown, not judged
    /// otherwise).
    pub fn judged(&self) -> bool {
        matches!(self.basis, Some(Basis::Loopback | Basis::Estimate))
    }

    /// A tendency may be claimed only on a measured loop (`Estimate` never).
    pub fn tendency_allowed(&self) -> bool {
        self.basis == Some(Basis::Loopback)
    }

    pub fn choices(&self) -> Choices {
        Choices {
            play_now: true,
            try_again: self.phase == Phase::Finished && !self.judged(),
        }
    }

    /// A new soundcheck starts: the screen comes up, the dots start empty.
    pub fn begin(&mut self) {
        self.phase = Phase::Running;
        self.shown = true;
        self.prompt = None;
        self.pictogram = Pictogram::Checking;
        self.dots = 0;
        self.outcome = None;
        self.skipped = false;
        self.bluetooth_hint = false;
    }

    /// The player plays now: the screen goes away, nothing is lost.
    pub fn skip(&mut self) -> SoundcheckReady {
        if self.phase != Phase::Finished {
            self.skipped = true;
        }
        self.shown = false;
        self.ready()
    }

    pub fn ready(&self) -> SoundcheckReady {
        SoundcheckReady {
            judged: self.judged(),
            basis: self.basis,
            outcome: self.outcome,
            skipped: self.skipped,
        }
    }

    fn fill(&mut self, stage: u8) {
        self.dots = self.dots.max(stage.min(DOTS));
    }

    /// Follows one session event. `Some` when the soundcheck just ended.
    pub fn on_event(&mut self, ev: &SessionEvent) -> Option<SoundcheckReady> {
        match ev {
            SessionEvent::Prompt(p) => {
                if self.phase != Phase::Running {
                    self.phase = Phase::Running;
                    self.dots = 0;
                }
                self.prompt = Some(*p);
                self.pictogram = pictogram_for(*p);
                self.fill(stage_of(*p));
                None
            }
            SessionEvent::SoundcheckDone { basis, outcome } => {
                self.basis = Some(*basis);
                self.outcome = Some(*outcome);
                self.phase = Phase::Finished;
                self.pictogram = match outcome {
                    SoundOutcome::Kept | SoundOutcome::Calibrated => {
                        self.fill(DOTS);
                        if self.judged() {
                            Pictogram::Ready
                        } else {
                            Pictogram::RhythmLater
                        }
                    }
                    SoundOutcome::Unstable => Pictogram::RhythmLater,
                    SoundOutcome::MicBlocked => Pictogram::MicBlocked,
                    SoundOutcome::Interrupted => Pictogram::Plug,
                };
                self.bluetooth_hint = !matches!(basis, Basis::Loopback);
                Some(self.ready())
            }
            SessionEvent::SoundcheckCancelled { basis } => {
                // Nothing was measured: the basis in force stays, the player can play.
                self.basis = Some(*basis);
                self.phase = Phase::Finished;
                self.outcome = None;
                self.pictogram = if self.judged() {
                    Pictogram::Ready
                } else {
                    Pictogram::RhythmLater
                };
                self.bluetooth_hint = !matches!(basis, Basis::Loopback);
                Some(self.ready())
            }
            _ => None,
        }
    }

    /// Follows the device link: a lost device is a plug and a way on, never an error.
    pub fn on_link(&mut self, link: Link) {
        if matches!(link, Link::Gone | Link::Unavailable)
            && self.phase != Phase::Idle
            && self.pictogram != Pictogram::Plug
        {
            self.phase = Phase::Finished;
            self.pictogram = Pictogram::Plug;
            self.bluetooth_hint = true;
        }
    }
}

/// What the player asks of the soundcheck (also sent by G3 from its own start screen).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundcheckCmd {
    /// Run it. `force`: measure again even if the saved setup holds.
    Start { force: bool },
    /// Hide the screen and let the round start.
    PlayNow,
}

/// The game may start a round: the soundcheck ended, or the player chose to play. Sent for
/// every outcome. `judged` says whether timing will be judged.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoundcheckReady {
    pub judged: bool,
    pub basis: Option<Basis>,
    pub outcome: Option<SoundOutcome>,
    /// The player did not wait for the end.
    pub skipped: bool,
}

/// Short pt-BR captions, drawn only when this is on (the maintainer decides).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoundcheckText(pub bool);

/// pt-BR caption of a pictogram.
pub fn caption(p: Pictogram) -> &'static str {
    match p {
        Pictogram::Waiting | Pictogram::Checking => "Conferindo o som",
        Pictogram::Silence => "Um instante de silêncio",
        Pictogram::TooLoud => "Afaste-se um pouco do alto-falante",
        Pictogram::Beeps { .. } => "Vão tocar alguns bipes",
        Pictogram::BeepsLouder { .. } => "Mais alguns bipes",
        Pictogram::MutedStrum { .. } => "Abafe as cordas e toque a cada clique",
        Pictogram::Ready => "Tudo pronto",
        Pictogram::RhythmLater => "O ritmo aparece; a avaliação fica para depois",
        Pictogram::MicBlocked => "Não ouvimos o microfone",
        Pictogram::Plug => "O áudio mudou",
    }
}

// ------------------------------------------------------------------ logic plugin

/// The logic half: state, commands, the optional first run.
pub struct SoundcheckPlugin {
    /// Send `Start { force: false }` on the first frame.
    pub auto_start: bool,
}

#[derive(Resource)]
struct AutoStart(bool);

impl Plugin for SoundcheckPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Soundcheck>()
            .init_resource::<AudioStatus>()
            .insert_resource(AutoStart(self.auto_start))
            .add_message::<SoundcheckCmd>()
            .add_message::<SoundcheckReady>()
            .add_message::<AudioCommand>()
            .add_message::<AudioNote>()
            .add_message::<AudioLink>()
            .add_systems(PreUpdate, handle_cmds.before(AudioSet))
            .add_systems(PreUpdate, follow.after(AudioSet));
    }
}

fn handle_cmds(
    mut auto: ResMut<AutoStart>,
    mut cmds: MessageReader<SoundcheckCmd>,
    mut sc: ResMut<Soundcheck>,
    status: Res<AudioStatus>,
    mut out: MessageWriter<AudioCommand>,
    mut ready: MessageWriter<SoundcheckReady>,
) {
    let mut todo: Vec<SoundcheckCmd> = cmds.read().copied().collect();
    if std::mem::take(&mut auto.0) {
        todo.insert(0, SoundcheckCmd::Start { force: false });
    }
    for c in todo {
        match c {
            SoundcheckCmd::Start { force } => {
                if status.link == Link::Gone {
                    out.write(AudioCommand::Retry);
                }
                sc.begin();
                out.write(AudioCommand::Soundcheck { force });
            }
            SoundcheckCmd::PlayNow => {
                if sc.phase == Phase::Running {
                    out.write(AudioCommand::CancelSoundcheck);
                }
                ready.write(sc.skip());
            }
        }
    }
}

fn follow(
    mut notes: MessageReader<AudioNote>,
    mut links: MessageReader<AudioLink>,
    status: Res<AudioStatus>,
    mut sc: ResMut<Soundcheck>,
    mut ready: MessageWriter<SoundcheckReady>,
) {
    for n in notes.read() {
        if let Some(r) = sc.on_event(&n.0) {
            ready.write(r);
        }
    }
    for l in links.read() {
        if matches!(l, AudioLink::Gone | AudioLink::Unavailable) {
            sc.on_link(status.link);
        }
    }
    sc.on_link(status.link);
}

/// Enter plays now, T tries again. Only while the screen is up (Space and Esc belong to the
/// round).
fn keys(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    sc: Res<Soundcheck>,
    mut cmds: MessageWriter<SoundcheckCmd>,
) {
    let (Some(keys), true) = (keys, sc.shown) else {
        return;
    };
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
        cmds.write(SoundcheckCmd::PlayNow);
    }
    if keys.just_pressed(KeyCode::KeyT) && sc.choices().try_again {
        cmds.write(SoundcheckCmd::Start { force: true });
    }
}

// ------------------------------------------------------------------ pictograms (data)

/// Ink of a shape. `Soft` is the theme tone; problems only use `White` and `Grey`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Soft,
    White,
    Grey,
}

impl Tone {
    pub fn color(self) -> Color {
        match self {
            Tone::Soft => palette::HIT,
            Tone::White => Color::srgb(1.0, 1.0, 1.0),
            Tone::Grey => Color::srgb(0.52, 0.52, 0.52),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Disc,
    /// Outline only.
    Ring,
    /// Rounded bar.
    Bar,
}

/// A shape in pixels, from the centre of the picture (y grows downward).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub kind: Kind,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub tone: Tone,
    /// Degrees, clockwise (bars of an arrow).
    pub rot: f32,
}

const fn shape(kind: Kind, x: f32, y: f32, w: f32, h: f32, tone: Tone) -> Shape {
    Shape {
        kind,
        x,
        y,
        w,
        h,
        tone,
        rot: 0.0,
    }
}

/// A speaker with three growing level bars: "listen". Centred on `x`, at `y`; `k` scales it.
fn listen_cue(y: f32, k: f32) -> Vec<Shape> {
    vec![
        shape(Kind::Bar, -29.0 * k, y, 14.0 * k, 14.0 * k, Tone::Soft),
        shape(Kind::Bar, -13.0 * k, y, 18.0 * k, 30.0 * k, Tone::Soft),
        shape(Kind::Bar, 5.0 * k, y, 6.0 * k, 14.0 * k, Tone::Soft),
        shape(Kind::Bar, 19.0 * k, y, 6.0 * k, 24.0 * k, Tone::Soft),
        shape(Kind::Bar, 33.0 * k, y, 6.0 * k, 34.0 * k, Tone::Soft),
    ]
}

/// A down arrow: "strum downward". Tip at `y_tip`.
fn down_cue(y_tip: f32) -> Vec<Shape> {
    // A solid triangle pointing down, as the ▼ of the play screen: stacked bars.
    (0..16)
        .map(|i| {
            let w = 72.0 * (1.0 - i as f32 / 16.0);
            shape(
                Kind::Bar,
                0.0,
                y_tip - 40.0 + i as f32 * 2.5,
                w,
                2.8,
                Tone::Soft,
            )
        })
        .collect()
}

/// A pictogram is a pure function of its state, so tests can enumerate it.
pub fn shapes_of(p: Pictogram) -> Vec<Shape> {
    use Kind::{Bar, Disc, Ring};
    use Tone::{Grey, Soft, White};
    let row = |n: u8, size: f32, gap: f32, y: f32, tone: Tone, filled: bool| -> Vec<Shape> {
        let n = n.clamp(1, 12) as f32;
        let total = n * size + (n - 1.0) * gap;
        (0..n as u32)
            .map(|i| {
                let x = -total / 2.0 + size / 2.0 + i as f32 * (size + gap);
                shape(if filled { Disc } else { Ring }, x, y, size, size, tone)
            })
            .collect()
    };
    match p {
        Pictogram::Waiting | Pictogram::Checking => row(3, 28.0, 22.0, 0.0, Soft, false),
        Pictogram::Silence => vec![
            shape(Ring, 0.0, 0.0, 96.0, 96.0, Soft),
            shape(Bar, 0.0, 0.0, 150.0, 4.0, Soft),
        ],
        Pictogram::TooLoud => {
            let mut v = vec![];
            for i in 0..5 {
                v.push(shape(Bar, -80.0 + i as f32 * 40.0, 0.0, 14.0, 110.0, Grey));
            }
            v.push(shape(Bar, 0.0, -66.0, 190.0, 5.0, White));
            v
        }
        // A speaker over one disc per beep.
        Pictogram::Beeps { count } => {
            let mut v = row(count, 34.0, 26.0, 28.0, Soft, true);
            v.extend(listen_cue(-52.0, 1.5));
            v
        }
        Pictogram::BeepsLouder { count } => {
            let mut v = row(count, 46.0, 24.0, 40.0, Soft, true);
            v.extend(listen_cue(-64.0, 1.9));
            v
        }
        Pictogram::MutedStrum { count_in, beat, of } => {
            let of = of.clamp(1, 8);
            let mut v = Vec::new();
            // A down arrow: the strum to make.
            v.extend(down_cue(-62.0));
            if beat == 0 && count_in > 0 {
                v.extend(row(count_in.min(8), 22.0, 16.0, -36.0, Soft, false));
            }
            for (i, s) in row(of, 40.0, 20.0, 28.0, Soft, true)
                .into_iter()
                .enumerate()
            {
                let lit = (i as u8) < beat;
                v.push(Shape {
                    kind: if lit { Disc } else { Ring },
                    ..s
                });
            }
            // The hand on the strings: a flat bar over the row.
            v.push(shape(Bar, 0.0, -4.0, 220.0, 8.0, White));
            v
        }
        Pictogram::Ready => vec![
            shape(Disc, 0.0, 0.0, 70.0, 70.0, Soft),
            shape(Ring, 0.0, 0.0, 120.0, 120.0, Soft),
        ],
        Pictogram::RhythmLater => {
            // Scattered hits: the rhythm is shown, not judged.
            let spots = [(-90.0, 10.0), (-30.0, -20.0), (25.0, 24.0), (85.0, -8.0)];
            let mut v: Vec<Shape> = spots
                .iter()
                .map(|(x, y)| shape(Disc, *x, *y, 30.0, 30.0, Grey))
                .collect();
            v.push(shape(Ring, 0.0, 0.0, 200.0, 100.0, Grey));
            v
        }
        Pictogram::MicBlocked => vec![
            shape(Bar, 0.0, -30.0, 40.0, 76.0, White),
            shape(Bar, 0.0, 30.0, 6.0, 40.0, Grey),
            shape(Bar, 0.0, 54.0, 70.0, 6.0, Grey),
            // The cover across it.
            shape(Bar, 0.0, -30.0, 120.0, 8.0, Grey),
        ],
        Pictogram::Plug => vec![
            shape(Bar, -70.0, 0.0, 90.0, 44.0, White),
            shape(Bar, 70.0, 0.0, 90.0, 44.0, Grey),
            shape(Bar, -30.0, 0.0, 22.0, 8.0, White),
            shape(Bar, 30.0, 0.0, 22.0, 8.0, Grey),
        ],
    }
}

/// The headphone for the Bluetooth hint: a band and two cups, grey, with a gap beside it.
pub fn bluetooth_shapes() -> Vec<Shape> {
    vec![
        shape(Kind::Bar, 0.0, -18.0, 56.0, 6.0, Tone::Grey),
        shape(Kind::Bar, -28.0, 0.0, 12.0, 30.0, Tone::White),
        shape(Kind::Bar, 28.0, 0.0, 12.0, 30.0, Tone::White),
        shape(Kind::Disc, 56.0, 0.0, 8.0, 8.0, Tone::Grey),
        shape(Kind::Disc, 74.0, 0.0, 8.0, 8.0, Tone::Grey),
    ]
}

/// Progress dots, left to right. Filled ones only ever increase.
pub fn dot_shapes(filled: u8) -> Vec<Shape> {
    (0..DOTS)
        .map(|i| {
            let x = (i as f32 - (DOTS as f32 - 1.0) / 2.0) * 26.0;
            if i < filled {
                shape(Kind::Disc, x, 0.0, 12.0, 12.0, Tone::Soft)
            } else {
                shape(Kind::Ring, x, 0.0, 12.0, 12.0, Tone::Grey)
            }
        })
        .collect()
}

/// The play-now button: a quiet pill. Its label ([`PLAY_NOW_LABEL`]) is drawn inside.
pub fn play_now_shapes() -> Vec<Shape> {
    vec![shape(Kind::Ring, 0.0, 0.0, 130.0, 34.0, Tone::Grey)]
}

/// What the pill says (pt-BR): the key and what it does. Always drawn: it is a control, not
/// a caption.
pub const PLAY_NOW_LABEL: &str = "Enter: pular";

// ------------------------------------------------------------------ drawing plugin

/// The drawing half.
pub struct SoundcheckDrawPlugin;

#[derive(Component)]
struct SoundcheckRoot;

/// Rebuilt children live here.
#[derive(Component)]
struct SoundcheckPanel;

impl Plugin for SoundcheckDrawPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SoundcheckText>()
            .init_resource::<Soundcheck>()
            .add_message::<SoundcheckCmd>()
            .add_systems(Startup, spawn_root)
            .add_systems(Update, (keys, redraw).chain());
    }
}

fn spawn_root(mut commands: Commands) {
    commands
        .spawn((
            SoundcheckRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.020, 0.027, 0.055, 0.92)),
            GlobalZIndex(50),
            Visibility::Hidden,
        ))
        .with_children(|p| {
            p.spawn((
                SoundcheckPanel,
                Node {
                    width: Val::Px(560.0),
                    height: Val::Px(380.0),
                    ..default()
                },
            ));
        });
}

fn add_shapes(p: &mut ChildSpawnerCommands, shapes: &[Shape], cx: f32, cy: f32) {
    for s in shapes {
        let color = s.tone.color();
        let radius = match s.kind {
            Kind::Disc | Kind::Ring => BorderRadius::MAX,
            Kind::Bar => BorderRadius::all(Val::Px(s.h.min(s.w) / 2.0)),
        };
        let mut node = Node {
            position_type: PositionType::Absolute,
            left: Val::Px(cx + s.x - s.w / 2.0),
            top: Val::Px(cy + s.y - s.h / 2.0),
            width: Val::Px(s.w),
            height: Val::Px(s.h),
            border_radius: radius,
            ..default()
        };
        let turn = UiTransform::from_rotation(Rot2::degrees(s.rot));
        if s.kind == Kind::Ring {
            node.border = UiRect::all(Val::Px(3.0));
            p.spawn((node, BorderColor::all(color), turn));
        } else {
            p.spawn((node, BackgroundColor(color), turn));
        }
    }
}

fn redraw(
    mut commands: Commands,
    sc: Res<Soundcheck>,
    text: Res<SoundcheckText>,
    mut root: Query<&mut Visibility, With<SoundcheckRoot>>,
    panel: Query<Entity, With<SoundcheckPanel>>,
) {
    if !sc.is_changed() && !text.is_changed() {
        return;
    }
    if let Ok(mut vis) = root.single_mut() {
        *vis = if sc.shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
    let Ok(panel) = panel.single() else {
        return;
    };
    let sc = sc.clone();
    let with_text = text.0;
    commands
        .entity(panel)
        .despawn_children()
        .with_children(|p| {
            add_shapes(p, &shapes_of(sc.pictogram), 280.0, 150.0);
            // Progress: a small row in the bottom-left corner, away from the cue.
            add_shapes(p, &dot_shapes(sc.dots), 60.0, 24.0);
            add_shapes(p, &play_now_shapes(), 480.0, 24.0);
            p.spawn((
                Text::new(PLAY_NOW_LABEL),
                TextFont {
                    font_size: FontSize::Px(15.0),
                    ..default()
                },
                TextColor(Color::srgb(0.80, 0.80, 0.80)),
                TextLayout::justify(Justify::Center),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(415.0),
                    top: Val::Px(14.0),
                    width: Val::Px(130.0),
                    ..default()
                },
            ));
            if sc.bluetooth_hint {
                add_shapes(p, &bluetooth_shapes(), 470.0, 350.0);
            }
            if with_text {
                p.spawn((
                    Text::new(caption(sc.pictogram)),
                    TextFont {
                        font_size: FontSize::Px(22.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.92, 0.92, 0.92)),
                    TextLayout::justify(Justify::Center),
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(0.0),
                        top: Val::Px(48.0),
                        width: Val::Px(560.0),
                        ..default()
                    },
                ));
            }
        });
}

/// Called once by `build_app` after the base plugins and resources are in place.
///
/// With a live or fake audio mode the soundcheck starts by itself (`--no-soundcheck`
/// leaves it to a [`SoundcheckCmd`]). Without audio (gallery, screenshots) it stays idle.
pub fn install(app: &mut App, cfg: &AppConfig) {
    let live = app
        .world()
        .get_resource::<AudioMode>()
        .is_some_and(|m| *m != AudioMode::None);
    let auto_start = live && !cfg.args.iter().any(|a| a == "--no-soundcheck");
    app.add_plugins((SoundcheckPlugin { auto_start }, SoundcheckDrawPlugin));
}
