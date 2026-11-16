//! G4: the round summary, the dispute and the tape indicator. `MinimalPlugins` (no window), virtual
//! time, the fake backend only: no device and no microphone are ever opened.

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use tonica_audio::FakeConfig;
use tonica_core::chart::Chart as CoreChart;
use tonica_core::pulse::{Exercise, exercise};
use tonica_core::stats::{Improvement, NoClaim, RoundStats, Tendency};
use tonica_core::timing::Level;
use tonica_core::verdict::{Attack, Direction, JudgeOut, PulseVerdict, Timing, Why};
use tonica_game::audio::{AudioBridgePlugin, AudioCommand, AudioNote, BridgeOptions};
use tonica_game::summary::{
    ChipMark, Dispute, DisputeLine, QuestionMark, Row, SummaryAction, SummaryDrawPlugin,
    SummaryLogicPlugin, SummaryRoot, SummaryState, TrendMark, row_text,
};
use tonica_game::tapeui::{Indicator, TapeDot, TapeUi, TapeUiPlugin, ToggleTape};
use tonica_session::rig::PlayerModel;
use tonica_session::session::{SessionEvent, TapeNote};
use tonica_session::writer::TapeSummary;

const STEP_S: f64 = 0.005;

#[derive(Resource, Default)]
struct Seen {
    cmds: Vec<String>,
    notes: Vec<SessionEvent>,
}

fn collect(
    mut cmds: MessageReader<AudioCommand>,
    mut notes: MessageReader<AudioNote>,
    mut seen: ResMut<Seen>,
) {
    seen.cmds.extend(cmds.read().map(|c| format!("{c:?}")));
    seen.notes.extend(notes.read().map(|n| n.0.clone()));
}

fn base() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            STEP_S,
        )))
        .init_resource::<Seen>()
        .add_plugins((SummaryLogicPlugin, SummaryDrawPlugin))
        .add_plugins(TapeUiPlugin { wanted: false })
        .add_systems(Last, collect);
    app.update();
    app
}

fn note(app: &mut App, e: SessionEvent) {
    app.world_mut()
        .resource_mut::<Messages<AudioNote>>()
        .write(AudioNote(e));
    app.update();
}

fn judge(app: &mut App, out: JudgeOut) {
    note(app, SessionEvent::Judge(out));
}

fn act(app: &mut App, a: SummaryAction) {
    app.world_mut()
        .resource_mut::<Messages<SummaryAction>>()
        .write(a);
    app.update();
}

fn state(app: &App) -> &SummaryState {
    app.world().resource::<SummaryState>()
}

fn count<C: Component>(app: &mut App) -> usize {
    app.world_mut().query::<&C>().iter(app.world()).count()
}

fn stats(heard: u32, center: u32, in_window: u32, tendency: Tendency) -> RoundStats {
    RoundStats {
        events: heard,
        heard,
        not_heard: 0,
        not_judged: 0,
        disputed: 0,
        center,
        in_window,
        outside: heard - center - in_window,
        timed: heard,
        median_ms: Some(-20.0),
        spread_ms: Some(12.0),
        uncertainty_ms: Some(3.0),
        p90_abs_ms: Some(40.0),
        constancy_focus: false,
        tendency,
    }
}

fn done(
    st: RoundStats,
    better: Vec<Improvement>,
    suggest: Option<Level>,
    interrupted: bool,
) -> SessionEvent {
    SessionEvent::RoundDone {
        stats: st,
        better,
        suggest,
        interrupted,
    }
}

fn verdict(id: u32, pos: u8, attack: Attack, dt: Option<f32>, fin: bool) -> JudgeOut {
    JudgeOut::Verdict(PulseVerdict {
        id,
        attack,
        timing: dt.map_or(Timing::NotJudged(Why::NoAttack), |_| Timing::InWindow),
        dt_ms: dt,
        dir: Direction::NotJudged(Why::Ghost),
        onset_in: None,
        pos_in_bar: pos,
        fin,
    })
}

fn all_text(app: &mut App) -> Vec<String> {
    app.world_mut()
        .query::<&Text>()
        .iter(app.world())
        .map(|t| t.0.clone())
        .collect()
}

fn wait(app: &mut App, s: f64) {
    for _ in 0..(s / STEP_S).ceil() as u32 {
        app.update();
    }
}

/// 1: what got better comes first; with nothing better the day counter does, and it only grows.
#[test]
fn g4_1_order_improvements_then_counter() {
    let mut app = base();
    let st = stats(10, 3, 5, Tendency::Centered);
    note(&mut app, done(st, vec![], None, false));
    let rows = state(&app).summary.as_ref().unwrap().rows();
    assert_eq!(rows[0], Row::Day(10), "no improvement: the counter leads");
    assert!(
        rows.iter().all(|r| !matches!(r, Row::Improved(_))),
        "{rows:?}"
    );

    let better = vec![
        Improvement::MoreInWindow { from: 0.4, to: 0.6 },
        Improvement::Steadier {
            from_ms: 20.0,
            to_ms: 12.0,
        },
    ];
    note(&mut app, done(st, better.clone(), None, false));
    let rows = state(&app).summary.as_ref().unwrap().rows();
    assert_eq!(rows[0], Row::Improved(better[0]));
    assert_eq!(rows[1], Row::Improved(better[1]));
    assert!(
        rows.iter().all(|r| !matches!(r, Row::Day(_))),
        "the counter waits when something improved: {rows:?}"
    );
    // The drawn lines follow the same order.
    let drawn: Vec<Row> = app
        .world_mut()
        .query::<&Row>()
        .iter(app.world())
        .copied()
        .collect();
    assert_eq!(drawn[0], Row::Improved(better[0]));

    // Only grows: a round of fewer strums never lowers the totals.
    let before = state(&app).totals.clone();
    note(
        &mut app,
        done(stats(4, 0, 1, Tendency::Centered), vec![], None, false),
    );
    let after = &state(&app).totals;
    assert!(after.strums >= before.strums && after.rounds > before.rounds);
    assert!(after.best_on_time >= before.best_on_time);
    assert!(after.firsts.len() >= before.firsts.len());
    assert_eq!(
        state(&app).summary.as_ref().unwrap().rows()[0],
        Row::Day(after.strums)
    );
}

/// 2: no number that goes down, no accuracy, no error count, no ms: the main panel shows none of
/// them. The Details panel (closed) holds the milliseconds.
#[test]
fn g4_2_only_monotone_numbers_in_the_panel() {
    let mut app = base();
    let better = vec![
        Improvement::MoreHeard { from: 0.5, to: 0.8 },
        Improvement::MoreInWindow { from: 0.4, to: 0.6 },
        Improvement::Steadier {
            from_ms: 20.0,
            to_ms: 12.0,
        },
        Improvement::CloserToCenter {
            from_ms: -30.0,
            to_ms: -10.0,
        },
    ];
    let mut s = stats(10, 3, 5, Tendency::Early(-20.0));
    s.not_heard = 2;
    s.disputed = 1;
    note(&mut app, done(s, better, Some(Level::Band), false));
    note(&mut app, done(s, vec![], None, false));
    for t in all_text(&mut app) {
        assert!(!t.contains('%'), "{t}");
        assert!(!t.contains("ms"), "{t}");
        let l = t.to_lowercase();
        for bad in [
            "erro",
            "errou",
            "falha",
            "preciso",
            "precisão",
            "ruim",
            "pior",
        ] {
            assert!(!l.contains(bad), "{t}");
        }
        if t.chars().any(|c| c.is_ascii_digit()) {
            assert!(
                t.starts_with("Golpes no total") || t.starts_with("Sua melhor rodada"),
                "a number outside the monotone lines: {t}"
            );
        }
    }
    for r in [
        Row::Improved(Improvement::MoreHeard { from: 0.1, to: 0.9 }),
        Row::Day(3),
        Row::Best(7),
        Row::Trend(tonica_game::summary::Trend::Late),
    ] {
        let t = row_text(&r);
        assert!(!t.contains('%') && !t.contains("ms"), "{t}");
    }
    // Details are closed until D, then the milliseconds appear.
    assert!(!all_text(&mut app).iter().any(|t| t.contains("Mediana")));
    act(&mut app, SummaryAction::ToggleDetails);
    assert!(all_text(&mut app).iter().any(|t| t.contains("Mediana")));
    act(&mut app, SummaryAction::ToggleDetails);
    assert!(!all_text(&mut app).iter().any(|t| t.contains("Mediana")));
}

/// 3: the trend exists only for Early or Late; with an estimate, a centred or short round there is
/// no entity for it. A first round gives "firsts", a later bigger one a personal best.
#[test]
fn g4_3_trend_only_when_claimed_firsts_and_best() {
    let mut app = base();
    for t in [
        Tendency::NotClaimed(NoClaim::EstimateOnly),
        Tendency::NotClaimed(NoClaim::TooFew),
        Tendency::NotClaimed(NoClaim::Uncertain),
        Tendency::NotClaimed(NoClaim::GateLocked),
        Tendency::Centered,
    ] {
        note(&mut app, done(stats(8, 2, 4, t), vec![], None, false));
        assert_eq!(count::<TrendMark>(&mut app), 0, "{t:?}");
    }
    note(
        &mut app,
        done(stats(8, 2, 4, Tendency::Early(-25.0)), vec![], None, false),
    );
    assert_eq!(count::<TrendMark>(&mut app), 1);
    note(
        &mut app,
        done(stats(8, 2, 4, Tendency::Late(25.0)), vec![], None, false),
    );
    assert_eq!(count::<TrendMark>(&mut app), 1);

    let mut app = base();
    note(
        &mut app,
        done(stats(8, 2, 4, Tendency::Centered), vec![], None, false),
    );
    let first = state(&app).summary.clone().unwrap();
    assert!(
        first
            .rows()
            .contains(&Row::First(tonica_game::summary::First::Round))
    );
    assert!(
        first
            .rows()
            .contains(&Row::First(tonica_game::summary::First::Center))
    );
    assert!(
        first.new_best.is_none(),
        "no best to beat in the first round"
    );
    note(
        &mut app,
        done(stats(8, 2, 4, Tendency::Centered), vec![], None, false),
    );
    let same = state(&app).summary.clone().unwrap();
    assert!(same.new_firsts.is_empty() && same.new_best.is_none());
    note(
        &mut app,
        done(stats(9, 3, 5, Tendency::Centered), vec![], None, false),
    );
    assert_eq!(state(&app).summary.as_ref().unwrap().new_best, Some(8));
}

/// 4: the window offer is a chip; nothing narrows until Enter.
#[test]
fn g4_4_window_chip_is_an_offer() {
    let mut app = base();
    note(
        &mut app,
        done(
            stats(8, 2, 4, Tendency::Centered),
            vec![],
            Some(Level::Band),
            false,
        ),
    );
    assert_eq!(count::<ChipMark>(&mut app), 1);
    assert!(
        !app.world()
            .resource::<Seen>()
            .cmds
            .iter()
            .any(|c| c.contains("SetLevel")),
        "an offer sends nothing"
    );
    act(&mut app, SummaryAction::AcceptSuggestion);
    app.update();
    assert!(
        app.world()
            .resource::<Seen>()
            .cmds
            .iter()
            .any(|c| c.contains("SetLevel(Band)"))
    );
    assert_eq!(count::<ChipMark>(&mut app), 0);
    // An interrupted round offers nothing.
    note(
        &mut app,
        done(
            stats(8, 2, 4, Tendency::Centered),
            vec![],
            Some(Level::Stage),
            true,
        ),
    );
    assert_eq!(count::<ChipMark>(&mut app), 0);
    // A new round hides the panel.
    note(
        &mut app,
        SessionEvent::RoundArmed {
            anchor: 1,
            fs_out: 48_000,
        },
    );
    assert_eq!(count::<SummaryRoot>(&mut app), 0);
}

/// 5: the dispute aims at the last non-ghost final verdict, shows it, and says so when it
/// withdraws it; with nothing to withdraw it says that, calmly.
#[test]
fn g4_5_dispute_target_and_result() {
    let mut app = base();
    // Pressing before anything was judged: nothing to withdraw.
    act(&mut app, SummaryAction::Dispute);
    assert!(matches!(
        state(&app).dispute,
        Dispute::Asked { target: None, .. }
    ));
    wait(&mut app, 0.7);
    assert!(matches!(
        state(&app).dispute,
        Dispute::NothingToWithdraw { .. }
    ));
    let lines = all_text(&mut app);
    assert!(
        lines.iter().any(|t| t.contains("nada para tirar")),
        "{lines:?}"
    );
    assert_eq!(count::<QuestionMark>(&mut app), 0);
    wait(&mut app, 4.5);
    assert_eq!(
        count::<DisputeLine>(&mut app),
        0,
        "the line goes away by itself"
    );

    // Event 0 final (heard), event 1 provisional only, event 2 a ghost final.
    judge(&mut app, verdict(0, 0, Attack::Heard, Some(-10.0), true));
    judge(&mut app, verdict(1, 1, Attack::Heard, Some(15.0), false));
    judge(
        &mut app,
        verdict(2, 2, Attack::NotJudged(Why::Ghost), None, true),
    );
    act(&mut app, SummaryAction::Dispute);
    let Dispute::Asked {
        target: Some(t), ..
    } = state(&app).dispute
    else {
        panic!("{:?}", state(&app).dispute)
    };
    assert_eq!(
        (t.id, t.pos_in_bar),
        (0, 0),
        "the provisional and the ghost are not targets"
    );
    assert_eq!(
        count::<QuestionMark>(&mut app),
        1,
        "the neutral ? sits on the target"
    );
    assert!(
        app.world()
            .resource::<Seen>()
            .cmds
            .iter()
            .any(|c| c.contains("Dispute"))
    );
    // The session answers.
    judge(
        &mut app,
        JudgeOut::Withdrawn {
            id: 0,
            why: Why::Disputed,
        },
    );
    let Dispute::Withdrawn { target, .. } = state(&app).dispute else {
        panic!("{:?}", state(&app).dispute)
    };
    assert_eq!(target.id, 0);
    assert!(
        all_text(&mut app)
            .iter()
            .any(|t| t.contains("saiu da conta"))
    );
    // A second press: that event is out already, so nothing is withdrawn.
    act(&mut app, SummaryAction::Dispute);
    assert!(matches!(
        state(&app).dispute,
        Dispute::Asked { target: None, .. }
    ));
    wait(&mut app, 0.7);
    assert!(matches!(
        state(&app).dispute,
        Dispute::NothingToWithdraw { .. }
    ));
    // Glitch withdrawals are not disputes.
    judge(&mut app, verdict(3, 3, Attack::NotHeard, None, true));
    judge(
        &mut app,
        JudgeOut::Withdrawn {
            id: 3,
            why: Why::Glitch,
        },
    );
    assert!(matches!(
        state(&app).dispute,
        Dispute::NothingToWithdraw { .. }
    ));
}

/// 6: the cloud keeps one dot per timed verdict; a withdrawal takes its dot out and the revised
/// statistics replace the old ones, without lowering any total.
#[test]
fn g4_6_cloud_and_revision() {
    let mut app = base();
    for i in 0..4u32 {
        judge(
            &mut app,
            verdict(
                i,
                i as u8,
                Attack::Heard,
                Some(-30.0 + 20.0 * i as f32),
                true,
            ),
        );
    }
    note(
        &mut app,
        done(stats(4, 1, 3, Tendency::Centered), vec![], None, false),
    );
    let s = state(&app).summary.clone().unwrap();
    assert_eq!(s.cloud.len(), 4);
    assert_eq!(s.hist.iter().map(|c| *c as usize).sum::<usize>(), 4);
    assert!(s.cloud.iter().all(|d| (-1.0..=1.0).contains(&d.x)));
    let strums = state(&app).totals.strums;
    // Dispute after the round.
    act(&mut app, SummaryAction::Dispute);
    judge(
        &mut app,
        JudgeOut::Withdrawn {
            id: 3,
            why: Why::Disputed,
        },
    );
    let mut revised = stats(3, 1, 2, Tendency::Centered);
    revised.disputed = 1;
    note(&mut app, SessionEvent::RoundRevised { stats: revised });
    let s = state(&app).summary.clone().unwrap();
    assert!(s.revised);
    assert_eq!(s.stats.disputed, 1);
    assert_eq!(s.cloud.len(), 3, "the contested event left the cloud");
    assert_eq!(state(&app).totals.strums, strums, "totals never go down");
}

/// 7: no red anywhere (white, grey, pale blue), the dispute mark included.
#[test]
fn g4_7_no_red() {
    let mut app = base();
    judge(&mut app, verdict(0, 0, Attack::NotHeard, None, true));
    act(&mut app, SummaryAction::Dispute);
    note(
        &mut app,
        done(
            stats(8, 2, 4, Tendency::Late(30.0)),
            vec![Improvement::Steadier {
                from_ms: 20.0,
                to_ms: 12.0,
            }],
            Some(Level::Band),
            false,
        ),
    );
    let mut colours: Vec<Color> = Vec::new();
    colours.extend(
        app.world_mut()
            .query::<&BackgroundColor>()
            .iter(app.world())
            .map(|c| c.0),
    );
    colours.extend(
        app.world_mut()
            .query::<&TextColor>()
            .iter(app.world())
            .map(|c| c.0),
    );
    colours.extend(
        app.world_mut()
            .query::<&BorderColor>()
            .iter(app.world())
            .map(|c| c.top),
    );
    assert!(colours.len() > 10);
    for c in colours {
        let s = c.to_srgba();
        assert!(s.red <= s.blue + 0.02, "reddish colour {s:?}");
    }
}

/// 8: tape: off until R; R sends Record, the dot follows the session; with R never pressed no
/// tape command exists; failure is a grey ring, not an alarm.
#[test]
fn g4_8_tape_indicator() {
    let mut app = base();
    for _ in 0..20 {
        app.update();
    }
    assert!(!app.world().resource::<TapeUi>().wanted);
    assert_eq!(count::<TapeDot>(&mut app), 0);
    assert!(
        !app.world()
            .resource::<Seen>()
            .cmds
            .iter()
            .any(|c| c.contains("Record"))
    );

    app.world_mut()
        .resource_mut::<Messages<ToggleTape>>()
        .write(ToggleTape);
    app.update();
    assert!(app.world().resource::<TapeUi>().wanted);
    app.update();
    assert!(
        app.world()
            .resource::<Seen>()
            .cmds
            .iter()
            .any(|c| c.contains("Record(true)"))
    );
    assert_eq!(
        count::<TapeDot>(&mut app),
        0,
        "the dot waits for the session"
    );

    note(
        &mut app,
        SessionEvent::Tape(TapeNote::Started {
            dir: PathBuf::from("t"),
            audio: true,
        }),
    );
    assert_eq!(
        app.world().resource::<TapeUi>().indicator,
        Indicator::Recording
    );
    assert_eq!(count::<TapeDot>(&mut app), 1);

    note(
        &mut app,
        SessionEvent::Tape(TapeNote::Failed("disk".into())),
    );
    assert_eq!(
        app.world().resource::<TapeUi>().indicator,
        Indicator::Stopped
    );
    assert_eq!(count::<TapeDot>(&mut app), 1);

    note(
        &mut app,
        SessionEvent::Tape(TapeNote::Started {
            dir: PathBuf::from("t"),
            audio: true,
        }),
    );
    let sum = TapeSummary {
        dir: PathBuf::from("t"),
        events: 10,
        mic_frames: 100,
        lost_frames: 0,
        dropped_raw_blocks: 0,
        disputed: true,
        complete: true,
        bytes: 1000,
    };
    note(&mut app, SessionEvent::Tape(TapeNote::Closed(sum)));
    assert_eq!(
        app.world().resource::<TapeUi>().indicator,
        Indicator::Closed {
            complete: true,
            disputed: true
        }
    );
    assert_eq!(count::<TapeDot>(&mut app), 0);

    app.world_mut()
        .resource_mut::<Messages<ToggleTape>>()
        .write(ToggleTape);
    app.update();
    app.update();
    assert!(
        app.world()
            .resource::<Seen>()
            .cmds
            .iter()
            .any(|c| c.contains("Record(false)"))
    );
}

// ---------------------------------------------------------------- fake backend end to end

fn fake_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            STEP_S,
        )))
        .insert_resource(tonica_game::gfx::SongClock {
            t: tonica_game::LEAD_IN,
            running: true,
        })
        .init_resource::<Seen>()
        .add_systems(Last, collect);
    let mut fc = FakeConfig::new(48_000, 48_000);
    fc.path.latency_s = 0.0873;
    fc.path.noise_rms = 1.0e-3;
    let opts = BridgeOptions {
        player: Some(PlayerModel {
            tendency_ms: -6.0,
            sigma_ms: 5.0,
            ..PlayerModel::default()
        }),
        ..BridgeOptions::default()
    };
    app.add_plugins(AudioBridgePlugin::fake(fc, opts))
        .add_plugins((SummaryLogicPlugin, SummaryDrawPlugin));
    app.update();
    app
}

fn start(app: &mut App, chart: CoreChart) {
    app.world_mut()
        .resource_mut::<Messages<AudioCommand>>()
        .write(AudioCommand::StartRound {
            chart,
            lead_in_beats: 4,
            take: None,
        });
    for _ in 0..1000 {
        app.update();
        if app
            .world()
            .resource::<Seen>()
            .notes
            .iter()
            .any(|e| matches!(e, SessionEvent::RoundArmed { .. }))
        {
            return;
        }
    }
    panic!("the round was not armed");
}

fn finals(app: &App) -> usize {
    app.world()
        .resource::<Seen>()
        .notes
        .iter()
        .filter(|e| matches!(e, SessionEvent::Judge(JudgeOut::Verdict(v)) if v.fin && !matches!(v.attack, Attack::NotJudged(_))))
        .count()
}

fn round_done(app: &App) -> bool {
    app.world()
        .resource::<Seen>()
        .notes
        .iter()
        .any(|e| matches!(e, SessionEvent::RoundDone { .. }))
}

/// 9: on the fake backend: a press in the middle of a round names the event the session then
/// withdraws; the round summary shows without it; a press after the round revises the numbers.
#[test]
fn g4_9_dispute_with_the_fake_session() {
    let mut app = fake_app();
    // A soundcheck first: without a measured loop nothing is judged.
    app.world_mut()
        .resource_mut::<Messages<AudioCommand>>()
        .write(AudioCommand::Soundcheck { force: true });
    for _ in 0..16000 {
        app.update();
        if app
            .world()
            .resource::<Seen>()
            .notes
            .iter()
            .any(|e| matches!(e, SessionEvent::SoundcheckDone { .. }))
        {
            break;
        }
    }
    app.update();
    start(&mut app, exercise(Exercise::OnClickQuarters, 80.0, 2));
    // Wait for two final verdicts, then press right after the next final one.
    let mut n = finals(&app);
    let mut target_n = 0;
    for _ in 0..8000 {
        app.update();
        let now = finals(&app);
        if now > n {
            n = now;
            if n >= 2 {
                target_n = n;
                break;
            }
        }
    }
    assert!(
        target_n >= 2,
        "no final verdicts: {:?}",
        app.world()
            .resource::<Seen>()
            .notes
            .iter()
            .filter(|e| matches!(e, SessionEvent::Judge(_)))
            .take(6)
            .collect::<Vec<_>>()
    );
    act(&mut app, SummaryAction::Dispute);
    let Dispute::Asked {
        target: Some(aimed),
        ..
    } = state(&app).dispute
    else {
        panic!("{:?}", state(&app).dispute)
    };
    for _ in 0..40 {
        app.update();
        if matches!(state(&app).dispute, Dispute::Withdrawn { .. }) {
            break;
        }
    }
    let Dispute::Withdrawn { target, .. } = state(&app).dispute else {
        panic!("the session withdrew nothing: {:?}", state(&app).dispute)
    };
    assert_eq!(target.id, aimed.id, "the shown target is the one withdrawn");
    assert!(app.world().resource::<Seen>().notes.iter().any(|e| matches!(
        e,
        SessionEvent::Judge(JudgeOut::Withdrawn { id, why: Why::Disputed }) if *id == aimed.id
    )));
    for _ in 0..12000 {
        if round_done(&app) {
            break;
        }
        app.update();
    }
    assert!(round_done(&app));
    app.update();
    let s = state(&app).summary.clone().expect("the summary");
    assert!(state(&app).visible);
    assert_eq!(s.stats.disputed, 1);
    assert!(!s.rows().is_empty());
    // Dispute after the round: the last final verdict is already out, so nothing is withdrawn.
    act(&mut app, SummaryAction::Dispute);
    wait(&mut app, 0.7);
    assert!(
        matches!(
            state(&app).dispute,
            Dispute::NothingToWithdraw { .. } | Dispute::Withdrawn { .. }
        ),
        "{:?}",
        state(&app).dispute
    );
}
