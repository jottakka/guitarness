//! Guitarness game: note-highway views (fretboard 3D, lanes 3D, 2D) and the Pulse mode.
//!
//! Keys: V cycles views, C switches demo/strokes chart, F toggles fret numerals.
//!
//! Convention for new modules: a logic plugin without rendering (tested with
//! `MinimalPlugins` and `TimeUpdateStrategy::ManualDuration`) plus a drawing plugin.
//! Each feature module exposes `install(&mut App, &AppConfig)`, which `build_app` calls.

pub mod audio;
pub mod chart;
pub mod contract;
pub mod fretboard;
pub mod gfx;
pub mod glyphs;
pub mod judge_fake;
pub mod learn;
pub mod palette;
pub mod pulse;
pub mod shots;
pub mod soundcheck;
pub mod summary;
pub mod tapeui;
pub mod view2d;
pub mod view3d;

use std::path::PathBuf;

// Declared in the manifest for units G1-G4; remove each line once its crate is used for real.
use guitarness_dsp as _;

use bevy::app::ScheduleRunnerPlugin;
use bevy::prelude::*;
use bevy::window::WindowResolution;

use chart::Chart;
use gfx::{ActiveView, ChartEntity, ShowFrets, SongClock, View, ViewCam};

pub const LEAD_IN: f32 = -2.0;

/// Chart switch requested by key C or the screenshot driver.
#[derive(Resource, Default)]
pub struct PendingChart(pub Option<&'static str>);

pub fn chart_by_name(name: &str) -> Chart {
    if name == "strokes" {
        Chart::gallery()
    } else {
        Chart::demo()
    }
}

/// Everything `main` reads from the command line.
#[derive(Clone, Debug)]
pub struct AppConfig {
    pub view: View,
    pub chart: &'static str,
    /// `--screenshot <dir>`: write the captures there and exit.
    pub shot_dir: Option<PathBuf>,
    /// `--ontop`: keep the window above others (off by default).
    pub ontop: bool,
    /// `--data <dir>`: keep the learner's profile there. Without it the Guest profile is used
    /// (memory only, nothing written).
    pub data: Option<PathBuf>,
    /// Arguments `main` did not recognise, for the feature modules (`audio::install`, ...).
    pub args: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            view: View::Fret3,
            chart: "demo",
            shot_dir: None,
            ontop: false,
            data: None,
            args: Vec::new(),
        }
    }
}

impl AppConfig {
    /// Parse the command line (without the program name).
    pub fn from_args(args: impl IntoIterator<Item = String>) -> AppConfig {
        let mut cfg = AppConfig::default();
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--view" => match it.next().as_deref() {
                    Some("2d") => cfg.view = View::D2,
                    Some("3d" | "fretboard") => cfg.view = View::Fret3,
                    Some("lanes") => cfg.view = View::D3,
                    other => eprintln!("--view expects fretboard|lanes|2d, got {other:?}"),
                },
                "--chart" => match it.next().as_deref() {
                    Some("strokes") => cfg.chart = "strokes",
                    Some("demo") => cfg.chart = "demo",
                    other => eprintln!("--chart expects strokes|demo, got {other:?}"),
                },
                "--screenshot" => cfg.shot_dir = it.next().map(PathBuf::from),
                "--ontop" => cfg.ontop = true,
                "--data" => cfg.data = it.next().map(PathBuf::from),
                _ => cfg.args.push(a),
            }
        }
        cfg
    }
}

/// Build the full app (window, views, feature modules) without running it.
pub fn build_app(cfg: AppConfig) -> App {
    let chart = chart_by_name(cfg.chart);
    let judge = judge_fake::FakeJudge::new(&chart);
    let shooting = cfg.shot_dir.is_some();

    let mut app = App::new();
    if shooting {
        // Screenshot mode never creates a window: no winit (no event loop, no focus), no
        // primary window, and a plain loop as runner. Cameras render to image targets.
        app.add_plugins((
            DefaultPlugins
                .build()
                .disable::<bevy::winit::WinitPlugin>()
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: bevy::window::ExitCondition::DontExit,
                    ..default()
                }),
            ScheduleRunnerPlugin::run_loop(std::time::Duration::from_millis(2)),
        ));
    } else {
        app.add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Guitarness".into(),
                window_level: if cfg.ontop {
                    bevy::window::WindowLevel::AlwaysOnTop
                } else {
                    bevy::window::WindowLevel::Normal
                },
                focused: true,
                resolution: WindowResolution::new(1280, 720).with_scale_factor_override(1.0),
                ..default()
            }),
            ..default()
        }));
    }
    install_ui_font(&mut app);
    app.insert_resource(ClearColor(palette::BG))
        .insert_resource(ActiveView(cfg.view))
        .insert_resource(ShowFrets(true))
        .insert_resource(PendingChart::default())
        .insert_resource(audio::AudioMode::default())
        .insert_resource(gfx::EvFade::default())
        .insert_resource(fretboard::Zone {
            lo: 1.0,
            hi: 4.0,
            snap: true,
        })
        .insert_resource(SongClock {
            t: if shooting { 0.0 } else { LEAD_IN },
            running: true,
        })
        .insert_resource(chart)
        .insert_resource(judge)
        .add_systems(Startup, (gfx::setup_assets, setup_scene).chain())
        .add_systems(
            Update,
            (
                advance_clock,
                keys,
                apply_chart,
                apply_view,
                gfx::ui_target,
                gfx::update_movers,
                gfx::update_event_fade,
                gfx::update_shocks,
                fretboard::update_zone,
                gfx::update_labels,
                fretboard::update_fret_nums,
                judge_fake::update_indicators,
                judge_fake::update_flash,
            )
                .chain(),
        );
    audio::install(&mut app, &cfg);
    soundcheck::install(&mut app, &cfg);
    pulse::install(&mut app, &cfg);
    summary::install(&mut app, &cfg);
    learn::install(&mut app, &cfg);
    contract::install(&mut app, &cfg);
    tapeui::install(&mut app, &cfg);
    if let Some(dir) = cfg.shot_dir {
        shots::install(&mut app, dir);
    }
    app
}

/// Bevy's built-in default font is an ASCII subset (no "ã", "ç", ...). Replace it with the full
/// Fira Mono Medium (SIL OFL, `fonts/FiraMono-LICENSE`) so every `TextFont` covers pt-BR.
fn install_ui_font(app: &mut App) {
    use bevy::text::Font;
    let font = Font::from_bytes(include_bytes!("../fonts/FiraMono-Medium.ttf").to_vec());
    let mut fonts = app.world_mut().resource_mut::<Assets<Font>>();
    fonts
        .insert(AssetId::default(), font)
        .expect("replace default font");
}

pub fn setup_scene(
    mut commands: Commands,
    chart: Res<Chart>,
    shapes: Res<gfx::Shapes>,
    mats: Res<gfx::Mats>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fade: ResMut<gfx::EvFade>,
) {
    fretboard::spawn_camera(&mut commands);
    fretboard::spawn_board(&mut commands, &shapes, &mats);
    view3d::spawn(&mut commands, &shapes, &mats);
    view2d::spawn(&mut commands, &shapes, &mats);
    for view in [View::Fret3, View::D3, View::D2] {
        glyphs::spawn_chart(
            &mut commands,
            view,
            &chart,
            &shapes,
            &mats,
            &mut materials,
            &mut fade,
        );
        glyphs::spawn_indicators(&mut commands, view, &shapes, &mats);
    }
}

fn advance_clock(
    time: Res<Time>,
    chart: Res<Chart>,
    shooting: Option<Res<shots::Shooting>>,
    mode: Res<audio::AudioMode>,
    mut clock: ResMut<SongClock>,
) {
    // With a real audio source the bridge owns the clock; the frame clock is for the gallery.
    if !clock.running || *mode != audio::AudioMode::None {
        return;
    }
    clock.t += time.delta_secs();
    if shooting.is_none() && clock.t > chart.length + 1.5 {
        clock.t = LEAD_IN;
    }
}

fn keys(
    keys: Res<ButtonInput<KeyCode>>,
    chart: Res<Chart>,
    mut view: ResMut<ActiveView>,
    mut show: ResMut<ShowFrets>,
    mut pending: ResMut<PendingChart>,
) {
    if keys.just_pressed(KeyCode::KeyV) {
        view.0 = view.0.next();
    }
    if keys.just_pressed(KeyCode::KeyF) {
        show.0 = !show.0;
    }
    if keys.just_pressed(KeyCode::KeyC) {
        pending.0 = Some(if chart.name == "demo" {
            "strokes"
        } else {
            "demo"
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_chart(
    mut commands: Commands,
    mut pending: ResMut<PendingChart>,
    old: Query<Entity, With<ChartEntity>>,
    shapes: Res<gfx::Shapes>,
    mats: Res<gfx::Mats>,
    mut clock: ResMut<SongClock>,
    mut zone: ResMut<fretboard::Zone>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fade: ResMut<gfx::EvFade>,
) {
    let Some(name) = pending.0.take() else { return };
    for e in &old {
        commands.entity(e).despawn();
    }
    let chart = chart_by_name(name);
    for view in [View::Fret3, View::D3, View::D2] {
        glyphs::spawn_chart(
            &mut commands,
            view,
            &chart,
            &shapes,
            &mats,
            &mut materials,
            &mut fade,
        );
    }
    commands.insert_resource(judge_fake::FakeJudge::new(&chart));
    commands.insert_resource(chart);
    clock.t = LEAD_IN;
    zone.snap = true;
}

fn apply_view(view: Res<ActiveView>, mut cams: Query<(&mut Camera, &ViewCam)>) {
    if !view.is_changed() {
        return;
    }
    for (mut cam, v) in &mut cams {
        cam.is_active = v.0 == view.0;
    }
}
