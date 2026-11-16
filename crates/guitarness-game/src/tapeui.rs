//! G4: the tape indicator and key R (plan G4, F6).
//!
//! A tape is the opt-in recording of a round. Key R turns it on and off; while a tape is open a
//! small quiet dot sits in a corner. Off by default: with R never pressed (and no `--tape`) no
//! `AudioCommand::Record(true)` is ever sent, so no tape exists. The dot is pale blue-white, never
//! red, and does not blink. When a tape stops for a reason that is not the player's, the dot
//! turns into a grey ring and nothing else changes: the session goes on.
//!
//! R is also G1's "try the device again" key while the link is `Gone`; in that state this module
//! leaves it alone. No file is touched here: the session writes the tape.

use bevy::prelude::*;

use guitarness_session::session::{SessionEvent, TapeNote};

use crate::AppConfig;
use crate::audio::{AudioCommand, AudioNote, AudioStatus, Link};

/// What the corner shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Indicator {
    /// No tape.
    #[default]
    Off,
    /// A tape is open.
    Recording,
    /// The last tape closed (`complete`: nothing was lost; `disputed`: a verdict was contested).
    Closed { complete: bool, disputed: bool },
    /// The tape stopped by itself (disk, queue): the round went on without it.
    Stopped,
}

/// What the player asked for and what the session reports.
#[derive(Resource, Debug, Default)]
pub struct TapeUi {
    /// The player wants tapes (key R, or `--tape`).
    pub wanted: bool,
    pub indicator: Indicator,
}

/// Key R, as a message (tests write it too).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToggleTape;

/// The dot in the corner; present only while the indicator is `Recording` or `Stopped`.
#[derive(Component)]
pub struct TapeDot(pub Indicator);

fn key(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    status: Option<Res<AudioStatus>>,
    mut out: MessageWriter<ToggleTape>,
) {
    let Some(k) = keys else { return };
    if status.is_some_and(|s| s.link == Link::Gone) {
        return; // R retries the device (G1)
    }
    if k.just_pressed(KeyCode::KeyR) {
        out.write(ToggleTape);
    }
}

fn toggle(
    mut taps: MessageReader<ToggleTape>,
    mut ui: ResMut<TapeUi>,
    mut cmds: MessageWriter<AudioCommand>,
) {
    for _ in taps.read() {
        ui.wanted = !ui.wanted;
        cmds.write(AudioCommand::Record(ui.wanted));
    }
}

fn notes(mut notes: MessageReader<AudioNote>, mut ui: ResMut<TapeUi>) {
    for AudioNote(ev) in notes.read() {
        if let SessionEvent::Tape(t) = ev {
            ui.indicator = match t {
                TapeNote::Started { .. } => Indicator::Recording,
                TapeNote::Closed(sum) => Indicator::Closed {
                    complete: sum.complete,
                    disputed: sum.disputed,
                },
                TapeNote::Failed(_) => Indicator::Stopped,
            };
        }
    }
}

const DOT: Color = Color::srgb(0.80, 0.92, 1.0);
const RING: Color = Color::srgb(0.52, 0.56, 0.64);

fn draw(
    mut commands: Commands,
    ui: Res<TapeUi>,
    old: Query<Entity, With<TapeDot>>,
    mut seen: Local<Option<Indicator>>,
) {
    if *seen == Some(ui.indicator) {
        return;
    }
    *seen = Some(ui.indicator);
    for e in &old {
        commands.entity(e).despawn();
    }
    let (filled, color) = match ui.indicator {
        Indicator::Recording => (true, DOT),
        Indicator::Stopped => (false, RING),
        Indicator::Off | Indicator::Closed { .. } => return,
    };
    commands.spawn((
        TapeDot(ui.indicator),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(16.0),
            top: Val::Px(16.0),
            width: Val::Px(10.0),
            height: Val::Px(10.0),
            border: UiRect::all(Val::Px(2.0)),
            border_radius: BorderRadius::MAX,
            ..default()
        },
        BackgroundColor(if filled { color } else { Color::NONE }),
        BorderColor::all(color),
    ));
}

/// State, key R and the dot.
pub struct TapeUiPlugin {
    /// Start with tapes wanted (`--tape`).
    pub wanted: bool,
}

impl Plugin for TapeUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<AudioCommand>()
            .add_message::<AudioNote>()
            .add_message::<ToggleTape>()
            .insert_resource(TapeUi {
                wanted: self.wanted,
                indicator: Indicator::Off,
            })
            .add_systems(Update, (key, toggle, notes, draw).chain());
    }
}

/// Called once by `build_app` after the base plugins and resources are in place.
pub fn install(app: &mut App, cfg: &AppConfig) {
    let wanted =
        cfg.args.iter().any(|a| a == "--tape") && !cfg.args.iter().any(|a| a == "--no-tape");
    app.add_plugins(TapeUiPlugin { wanted });
}
