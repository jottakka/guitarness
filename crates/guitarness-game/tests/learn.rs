//! W1: the learner inside the game. `MinimalPlugins`, virtual time, synthetic judge outputs:
//! no window, no audio device, no microphone.

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use guitarness_core::pulse::expectations;
use guitarness_core::stats::{RoundStats, Tendency};
use guitarness_core::verdict::{Attack, Direction, JudgeOut, PulseVerdict, Timing, Why};
use guitarness_game::audio::{AudioLink, AudioNote};
use guitarness_game::gfx::SongClock;
use guitarness_game::learn::{BlockFinished, LearnLogicPlugin, Learner, OfferAnswered};
use guitarness_game::pulse::flow::{
    Offer, Offers, Phase, PulseAction, PulseFlow, PulseLogicPlugin, build_chart_bars,
};
use guitarness_game::summary::{
    ElseKey, OfferMark, Row, SummaryDrawPlugin, SummaryLogicPlugin, SummaryRoot, SummaryState,
};
use guitarness_learn::types::{Axis, BlockKind, BlockSpec, Choice, LogLine, ProfileId};
use guitarness_session::session::SessionEvent;

const STEP_S: f64 = 0.005;
const DAY: i64 = 86_400_000;
/// 2026-01-05, a Monday, 09:00 UTC.
const T0: i64 = 1_767_603_600_000;

#[derive(Clone, Copy, PartialEq)]
enum Play {
    /// Every strum heard and in the window.
    Good,
    /// Every strum heard, outside the window.
    Bad,
}

fn app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            STEP_S,
        )))
        .insert_resource(SongClock {
            t: -2.0,
            running: false,
        })
        .add_message::<AudioLink>()
        .add_plugins((LearnLogicPlugin, PulseLogicPlugin))
        .add_plugins((SummaryLogicPlugin, SummaryDrawPlugin));
    app.world_mut().resource_mut::<Learner>().clock_ms = Some(T0);
    app.world_mut().resource_mut::<PulseFlow>().screen_on = true;
    app.update();
    app
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("guitarness-w1-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn set_clock(app: &mut App, ms: i64) {
    app.world_mut().resource_mut::<Learner>().clock_ms = Some(ms);
}

fn learner(app: &App) -> &Learner {
    app.world().resource::<Learner>()
}

fn flow(app: &App) -> &PulseFlow {
    app.world().resource::<PulseFlow>()
}

fn note(app: &mut App, e: SessionEvent) {
    app.world_mut()
        .resource_mut::<Messages<AudioNote>>()
        .write(AudioNote(e));
}

fn act(app: &mut App, a: PulseAction) {
    app.world_mut()
        .resource_mut::<Messages<PulseAction>>()
        .write(a);
    app.update();
}

fn stats(heard: u32, outside: u32) -> RoundStats {
    RoundStats {
        events: heard,
        heard,
        not_heard: 0,
        not_judged: 0,
        disputed: 0,
        center: 0,
        in_window: heard - outside,
        outside,
        timed: heard,
        median_ms: Some(-20.0),
        spread_ms: Some(12.0),
        uncertainty_ms: Some(3.0),
        p90_abs_ms: Some(40.0),
        constancy_focus: false,
        tendency: Tendency::Centered,
    }
}

/// Press play, deliver a full round of judge outputs and the round end. `secs` of the clock
/// pass inside the round. Returns the spec that was played.
fn round(app: &mut App, how: Play, secs: i64) -> BlockSpec {
    act(app, PulseAction::Play);
    let spec = learner(app).active.clone().expect("a block began").spec;
    let chart = build_chart_bars(flow(app), Some(u32::from(spec.bars)));
    let ex = expectations(&chart);
    let t = learner(app).now_ms();
    set_clock(app, t + secs * 1000);
    for e in &ex {
        let (timing, dt) = match how {
            Play::Good => (Timing::InWindow, 12.0),
            Play::Bad => (Timing::Outside, 95.0),
        };
        let v = PulseVerdict {
            id: e.id,
            attack: Attack::Heard,
            timing,
            dt_ms: Some(dt),
            dir: Direction::NotJudged(Why::Ghost),
            onset_in: None,
            pos_in_bar: ((e.tick % 1920) / 240) as u8,
            fin: true,
        };
        note(app, SessionEvent::Judge(JudgeOut::Verdict(v)));
    }
    let n = ex.len() as u32;
    let out = if how == Play::Bad { n } else { 0 };
    note(
        app,
        SessionEvent::RoundDone {
            stats: stats(n, out),
            better: Vec::new(),
            suggest: None,
            interrupted: false,
        },
    );
    app.update();
    app.update();
    spec
}

/// Every number the player can see, in one list. None may ever go down.
fn shown(app: &App) -> Vec<u64> {
    let d = &learner(app).display;
    let mut v = vec![
        d.strums.get(),
        d.blocks.get(),
        d.active_ms.get(),
        d.days.get(),
        d.best_block_strums.get(),
        d.firsts.len() as u64,
    ];
    v.extend(d.best_bpm.values().map(|m| m.get()));
    let st = app.world().resource::<SummaryState>();
    v.push(st.totals.strums.into());
    if let Some(s) = &st.summary {
        for r in s.rows() {
            if let Row::Day(n) = r {
                v.push(n.into());
            }
        }
    }
    v
}

fn day_row(app: &App) -> Option<u32> {
    let st = app.world().resource::<SummaryState>();
    st.summary
        .as_ref()?
        .rows()
        .into_iter()
        .find_map(|r| match r {
            Row::Day(n) => Some(n),
            _ => None,
        })
}

fn axes_changed(a: BlockSpec, b: BlockSpec) -> u32 {
    u32::from(a.exercise != b.exercise) + u32::from(a.bpm != b.bpm)
}

// ------------------------------------------------------------------ tests

#[test]
fn bad_rounds_and_a_dispute_never_lower_a_shown_number() {
    let mut app = app();
    round(&mut app, Play::Good, 40);
    let mut prev = shown(&app);
    let mut prev_day = day_row(&app);
    let mut rows = 0;
    for (i, how) in [Play::Bad, Play::Bad, Play::Bad].into_iter().enumerate() {
        set_clock(&mut app, T0 + (i as i64 + 1) * 3_600_000);
        round(&mut app, how, 40);
        let now = shown(&app);
        assert!(
            now.len() >= prev.len() && prev.iter().zip(&now).all(|(a, b)| b >= a),
            "a number fell: {prev:?} -> {now:?}"
        );
        if let (Some(a), Some(b)) = (prev_day, day_row(&app)) {
            assert!(b >= a, "the counter fell");
            rows += 1;
        }
        prev = now;
        prev_day = day_row(&app);
    }
    assert!(rows >= 2, "the summary counter was never compared");
    // A contest after the last block: the books are folded again, the display never changes.
    let before = shown(&app);
    let lines_before = learner(&app).store.lines().len();
    note(
        &mut app,
        SessionEvent::Judge(JudgeOut::Withdrawn {
            id: 3,
            why: Why::Disputed,
        }),
    );
    app.update();
    assert!(
        learner(&app)
            .store
            .lines()
            .iter()
            .any(|l| matches!(l, LogLine::Dispute { id: 3, .. })),
        "the dispute is a log line"
    );
    assert_eq!(learner(&app).store.lines().len(), lines_before + 1);
    let after = shown(&app);
    assert_eq!(after, before, "contesting changed a shown number");
}

#[test]
fn after_a_probe_there_is_no_summary() {
    let dir = tmp("probe");
    let id = ProfileId::Local("00000000deadbeef".into());
    // Two good blocks, then the app closes.
    {
        let mut l = Learner::open_local(&dir, &id).unwrap();
        l.clock_ms = Some(T0);
        let mut app = app();
        app.insert_resource(l);
        round(&mut app, Play::Good, 40);
        let t = learner(&app).now_ms();
        set_clock(&mut app, t + 3_600_000);
        round(&mut app, Play::Good, 40);
        assert!(learner(&app).is_persistent());
        learner(&app).flush();
    }
    // Three days later the stored profile is opened again.
    let mut app = app();
    let mut l = Learner::open_local(&dir, &id).unwrap();
    l.clock_ms = Some(T0 + 3 * DAY);
    let (best_before, firsts_before) = (l.display.best_bpm.clone(), l.display.firsts.clone());
    let strums_before = l.display.strums.get();
    app.insert_resource(l);
    app.update();
    let l = learner(&app);
    assert_eq!(
        l.plan().map(|p| p.kind),
        Some(BlockKind::Probe),
        "a probe is due after the gap"
    );
    assert!(!l.plan().unwrap().show_summary);
    round(&mut app, Play::Good, 40);
    let st = app.world().resource::<SummaryState>();
    assert!(
        st.summary.is_none() && !st.visible,
        "a probe built a summary"
    );
    assert!(
        app.world_mut()
            .query::<&SummaryRoot>()
            .iter(app.world())
            .next()
            .is_none(),
        "a probe left a summary entity"
    );
    let l = learner(&app);
    assert!(l.display.strums.get() > strums_before, "totals still grow");
    assert_eq!(
        l.display.best_bpm, best_before,
        "a probe never moves a mark"
    );
    assert_eq!(
        l.display.firsts, firsts_before,
        "a probe never adds a first"
    );
    // The next block shows its summary again.
    set_clock(&mut app, T0 + 3 * DAY + 3_600_000);
    round(&mut app, Play::Good, 40);
    assert!(app.world().resource::<SummaryState>().summary.is_some());
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_good_player_over_three_days_is_offered_one_axis_at_a_time() {
    let mut app = app();
    let mut played: Vec<BlockSpec> = Vec::new();
    let mut tempo_steps = 0;
    let mut rhythm_steps = 0;
    for day in 0..3 {
        for k in 0..4 {
            set_clock(&mut app, T0 + day * DAY + k * 1_800_000);
            // The offer the player meets, with its single chip.
            let offers = flow(&app).offers;
            assert!(!(offers.up && offers.down), "two chips at once");
            let change = learner(&app).change();
            assert_eq!(
                offers.up,
                matches!(change, Some((Axis::Tempo, _, _))),
                "the tempo chip is the learner's tempo change, nothing else"
            );
            if offers.up {
                let (_, old, new) = change.unwrap();
                assert!(new > old, "an offer never goes down");
                assert_eq!(flow(&app).bpm, f64::from(old), "the chip waits for a press");
                act(&mut app, PulseAction::Accept(Offer::TempoUp));
                assert_eq!(flow(&app).bpm, f64::from(new));
                assert_eq!(
                    flow(&app).offers,
                    Offers {
                        up: false,
                        down: true
                    },
                    "the way back is the only chip left"
                );
                tempo_steps += 1;
            }
            let spec = round(&mut app, Play::Good, 40);
            // Plan J4: a block differs from the comfort of its own skill (the last block of
            // that rhythm, or the starter the first time) in at most one axis.
            let comfort = played
                .iter()
                .rev()
                .find(|p| p.exercise == spec.exercise)
                .copied()
                .unwrap_or(learner(&app).cfg.starter);
            if !played.is_empty() && played.iter().all(|p| p.exercise != spec.exercise) {
                assert!(matches!(change, Some((Axis::Rhythm, _, _))), "{change:?}");
                rhythm_steps += 1;
            }
            assert!(
                axes_changed(comfort, spec) <= 1,
                "two axes at once: {comfort:?} -> {spec:?}"
            );
            played.push(spec);
        }
    }
    assert!(
        tempo_steps >= 1 && played.iter().any(|s| s.bpm > 60),
        "a good player never got a step: {played:?}"
    );
    assert!(rhythm_steps <= 1, "one new rhythm at most in three days");
    // Every offer in the log changes at most one axis against the player's last block.
    let lines = learner(&app).store.lines();
    assert!(lines.iter().any(|l| matches!(l, LogLine::Offer(_))));
    assert!(lines.iter().any(|l| matches!(l, LogLine::Block(_))));
}

#[test]
fn something_else_moves_to_the_next_candidate_and_is_logged() {
    let mut app = app();
    for k in 0..6 {
        set_clock(&mut app, T0 + k * 1_800_000);
        if learner(&app)
            .offer
            .as_ref()
            .is_some_and(|o| o.cands.len() >= 2)
        {
            break;
        }
        round(&mut app, Play::Good, 40);
    }
    let l = learner(&app);
    let o = l.offer.clone().expect("an offer");
    let rev = l.offer_rev;
    app.world_mut()
        .resource_mut::<Messages<OfferAnswered>>()
        .write(OfferAnswered(Choice::SomethingElse));
    app.update();
    let l = learner(&app);
    let now = l.offer.as_ref().unwrap();
    assert_ne!(l.offer_rev, rev, "the offer did not change");
    assert_eq!(now.chosen, (o.chosen + 1) % o.cands.len());
    assert!(
        l.store
            .lines()
            .iter()
            .any(|l| matches!(l, LogLine::Choice(Choice::SomethingElse)))
    );
    // The screen follows: one offer at a time.
    assert_eq!(
        flow(&app).offers.up,
        matches!(l.change(), Some((Axis::Tempo, _, _)))
    );
    assert!(!flow(&app).offers.down);
    assert_eq!(flow(&app).phase, Phase::Between);
}

#[test]
fn the_guest_profile_touches_no_disk() {
    let dir = tmp("guest");
    let mut app = app();
    for k in 0..3 {
        set_clock(&mut app, T0 + k * 1_800_000);
        round(&mut app, Play::Good, 40);
    }
    let l = learner(&app);
    assert!(!l.is_persistent() && !l.store.is_persistent());
    assert_eq!(l.write_errors(), 0);
    assert!(
        l.store
            .lines()
            .iter()
            .any(|x| matches!(x, LogLine::Block(_)))
    );
    assert!(l.display.strums.get() > 0, "the books still work in memory");
    let entries = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(entries, 0, "the data folder was touched");
    assert!(!dir.join("profiles").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stored_profile_is_written_off_the_frame_and_reloads_the_same() {
    let dir = tmp("local");
    let id = ProfileId::Local("00000000cafe0001".into());
    let (ledger_mean, strums, blocks);
    {
        let mut app = app();
        app.insert_resource(Learner::open_local(&dir, &id).unwrap());
        let mut finished = 0;
        for k in 0..3 {
            set_clock(&mut app, T0 + k * 1_800_000);
            round(&mut app, Play::Good, 40);
            finished += app
                .world_mut()
                .resource_mut::<Messages<BlockFinished>>()
                .drain()
                .count();
        }
        assert!(finished >= 1);
        let l = learner(&app);
        l.flush();
        assert_eq!(l.write_errors(), 0);
        let key = guitarness_learn::types::SkillKey("pulse:quarter".into());
        ledger_mean = l.ledger.mean(&key);
        strums = l.display.strums.get();
        blocks = l.display.blocks.get();
    }
    let pdir = dir.join("profiles").join("00000000cafe0001");
    assert!(pdir.join("learn.jsonl").exists());
    assert!(pdir.join("display.json").exists());
    let l = Learner::open_local(&dir, &id).unwrap();
    let key = guitarness_learn::types::SkillKey("pulse:quarter".into());
    assert_eq!(l.ledger.mean(&key), ledger_mean);
    assert_eq!(l.display.strums.get(), strums);
    assert_eq!(l.display.blocks.get(), blocks);
    drop(l);
    let _ = std::fs::remove_dir_all(&dir);
}

fn offer_lines(app: &App) -> usize {
    learner(app)
        .store
        .lines()
        .iter()
        .filter(|l| matches!(l, LogLine::Offer(_)))
        .count()
}

#[test]
fn stopping_a_round_logs_exactly_one_offer_line() {
    let mut app = app();
    round(&mut app, Play::Good, 40);
    let before = offer_lines(&app);
    // Play, then stop before the round end arrives; frames pass in between.
    act(&mut app, PulseAction::Play);
    app.world_mut().resource_mut::<SongClock>().t = 1.0;
    app.update();
    act(&mut app, PulseAction::Stop);
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(offer_lines(&app), before, "an offer was logged mid-stop");
    note(
        &mut app,
        SessionEvent::RoundDone {
            stats: stats(2, 0),
            better: Vec::new(),
            suggest: None,
            interrupted: true,
        },
    );
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(offer_lines(&app), before + 1, "one offer line per stop");
}

#[test]
fn the_offer_is_one_pictogram_and_the_else_key_a_keycap_with_no_sentence() {
    let mut app = app();
    round(&mut app, Play::Good, 40);
    assert!(app.world().resource::<SummaryState>().visible);
    for axis in [Axis::Rhythm, Axis::Tempo] {
        {
            let mut l = app.world_mut().resource_mut::<Learner>();
            let o = l.offer.as_mut().expect("an offer");
            o.change = Some((axis, 80, 85));
        }
        app.world_mut()
            .resource_mut::<Messages<guitarness_game::learn::OfferChanged>>()
            .write(guitarness_game::learn::OfferChanged);
        app.update();
        let w = app.world_mut();
        let marks: Vec<Axis> = w.query::<&OfferMark>().iter(w).map(|m| m.0).collect();
        assert_eq!(marks, vec![axis], "one pictogram for the one change");
        let texts: Vec<String> = w.query::<&Text>().iter(w).map(|t| t.0.clone()).collect();
        for gone in ["Próximo", "opcional", "outra coisa", "colcheias"] {
            assert!(
                !texts.iter().any(|t| t.contains(gone)),
                "sentence {gone:?} still drawn: {texts:?}"
            );
        }
    }
    // No change on the offer: no pictogram.
    app.world_mut()
        .resource_mut::<Learner>()
        .offer
        .as_mut()
        .unwrap()
        .change = None;
    app.update();
    let w = app.world_mut();
    assert_eq!(w.query::<&OfferMark>().iter(w).count(), 0);
    let can_else = w
        .resource::<Learner>()
        .offer
        .as_ref()
        .is_some_and(|o| o.cands.len() >= 2);
    assert_eq!(w.query::<&ElseKey>().iter(w).count(), usize::from(can_else));
}
