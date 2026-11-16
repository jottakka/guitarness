//! Round statistics, tendency and tolerance (plan §5, unit C2: test 10 and the statistics
//! side of the rest). Verdicts are built by hand; the last test runs the judge.

use tonica_core::gates::{GateEntry, GateEvidence, GateRule, GateState, GateTable};
use tonica_core::judge::{JudgeConfig, PulseJudge};
use tonica_core::stats::{
    Improvement, NoClaim, RoundStats, Tendency, VerdictLog, tolerance_suggestion,
};
use tonica_core::timing::{Level, TimingRef};
use tonica_core::verdict::{
    Attack, Direction, ExpectKind, Judge, JudgeInput, JudgeOut, PulseVerdict, Timing, VerdictType,
    Why,
};

/// `u_t = K · MAD / √n + u`.
const K: f64 = 2.0 * 1.2533 * 1.4826;

/// A measured loop with `u = 3.0` exactly.
fn loop_ref() -> TimingRef {
    TimingRef::loopback(80.0, 0.0)
}

fn timed(id: u32, dt: f64, timing: Timing) -> PulseVerdict {
    PulseVerdict {
        id,
        attack: Attack::Heard,
        timing,
        dt_ms: Some(dt as f32),
        dir: Direction::NotJudged(Why::GateLocked),
        onset_in: Some(f64::from(id)),
        pos_in_bar: 0,
        fin: true,
    }
}

fn hit(id: u32, dt: f64) -> PulseVerdict {
    timed(id, dt, Timing::InWindow)
}

fn not_heard(id: u32) -> PulseVerdict {
    PulseVerdict {
        attack: Attack::NotHeard,
        timing: Timing::NotJudged(Why::NoAttack),
        dt_ms: None,
        onset_in: None,
        ..hit(id, 0.0)
    }
}

fn void(id: u32, why: Why) -> PulseVerdict {
    PulseVerdict {
        attack: Attack::NotJudged(why),
        timing: Timing::NotJudged(why),
        dt_ms: None,
        ..hit(id, 0.0)
    }
}

/// `n` (even) timed verdicts with median `m` and MAD `d`: half at `m − d`, half at `m + d`.
fn two_point(n: u32, m: f64, d: f64) -> Vec<PulseVerdict> {
    assert!(n.is_multiple_of(2));
    (0..n)
        .map(|k| hit(k, if k % 2 == 0 { m - d } else { m + d }))
        .collect()
}

fn hits(dts: &[f64]) -> Vec<PulseVerdict> {
    dts.iter()
        .enumerate()
        .map(|(k, &dt)| hit(k as u32, dt))
        .collect()
}

fn tendency(v: &[PulseVerdict], t: &TimingRef) -> Tendency {
    RoundStats::from_verdicts(v, t).tendency
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
    fn normal(&mut self) -> f64 {
        (0..12).map(|_| self.unit()).sum::<f64>() - 6.0
    }
}

// ---------------------------------------------------------------------------------------
// (10) Tendency

#[test]
fn t10_the_five_outcomes_at_their_edges() {
    let t = loop_ref();

    // 1. No measured loop: never claimed, however clear the numbers.
    let v = two_point(32, -30.0, 5.0);
    for no_loop in [
        TimingRef::estimate(100.0, 5.0, 16),
        TimingRef::estimate(100.0, 0.0, 400),
        TimingRef::none(),
        TimingRef::loopback(f64::NAN, 0.0),
    ] {
        assert_eq!(
            tendency(&v, &no_loop),
            Tendency::NotClaimed(NoClaim::EstimateOnly)
        );
    }
    match tendency(&v, &t) {
        Tendency::Early(m) => assert!(close(m, -30.0)),
        other => panic!("{other:?}"),
    }
    // ...and that comes before "too few"
    assert_eq!(
        tendency(&[], &TimingRef::none()),
        Tendency::NotClaimed(NoClaim::EstimateOnly)
    );

    // 2. Too few: 11 timed attacks are not enough, 12 are.
    let mut v = two_point(10, -30.0, 5.0);
    v.push(hit(90, -30.0));
    assert_eq!(tendency(&v, &t), Tendency::NotClaimed(NoClaim::TooFew));
    v.push(hit(91, -30.0));
    assert!(matches!(tendency(&v, &t), Tendency::Early(_)));
    assert_eq!(tendency(&[], &t), Tendency::NotClaimed(NoClaim::TooFew));

    // 3. Uncertain: u_t = K·d/√16 + 3 crosses 15 at d0.
    let d0 = (15.0 - 3.0) * 4.0 / K;
    let s = RoundStats::from_verdicts(&two_point(16, 0.0, d0 * 0.999), &t);
    assert_eq!(s.tendency, Tendency::Centered);
    assert!(s.uncertainty_ms.unwrap() < 15.0 && s.uncertainty_ms.unwrap() > 14.98);
    for m in [0.0, -60.0, 60.0] {
        assert_eq!(
            tendency(&two_point(16, m, d0 * 1.001), &t),
            Tendency::NotClaimed(NoClaim::Uncertain),
            "m {m}"
        );
    }
    // the uncertainty of the loop is part of it: the same strums, a looser loop
    let d = 11.0 * 4.0 / K; // u_t = 14 with u = 3
    assert_eq!(tendency(&two_point(16, 0.0, d), &t), Tendency::Centered);
    assert_eq!(
        tendency(&two_point(16, 0.0, d), &TimingRef::loopback(80.0, 2.0)),
        Tendency::NotClaimed(NoClaim::Uncertain)
    );

    // 4 and 5. Centered up to max(10, u_t), early or late beyond.
    // u_t = 4.86 < 10: the edge is 10 ms.
    let at = |m: f64| tendency(&two_point(16, m, 2.0), &t);
    assert_eq!(at(9.99), Tendency::Centered);
    assert_eq!(at(-9.99), Tendency::Centered);
    assert!(matches!(at(10.01), Tendency::Late(m) if close(m, 10.01)));
    assert!(matches!(at(-10.01), Tendency::Early(m) if close(m, -10.01)));
    // u_t = 12.29 > 10: the edge is u_t. A fixed 10 ms would call 11 ms "late".
    let u_t = K * 10.0 / 4.0 + 3.0;
    let at = |m: f64| tendency(&two_point(16, m, 10.0), &t);
    assert_eq!(at(11.0), Tendency::Centered);
    assert_eq!(at(-11.0), Tendency::Centered);
    assert_eq!(at(u_t - 0.01), Tendency::Centered);
    assert!(matches!(at(u_t + 0.01), Tendency::Late(_)));
    assert!(matches!(at(-u_t - 0.01), Tendency::Early(_)));

    // the numbers behind it
    let s = RoundStats::from_verdicts(&two_point(16, -22.0, 10.0), &t);
    assert!(close(s.median_ms.unwrap(), -22.0));
    assert!(close(s.spread_ms.unwrap(), 14.826));
    assert!(close(s.uncertainty_ms.unwrap(), u_t));
    assert_eq!(s.timed, 16);
}

/// The rule of plan §2.1, written again from the text.
fn rule(e: &[f64], u: f64) -> &'static str {
    let n = e.len();
    if n < 12 {
        return "too few";
    }
    let med = |x: &mut Vec<f64>| {
        x.sort_by(f64::total_cmp);
        (x[(x.len() - 1) / 2] + x[x.len() / 2]) / 2.0
    };
    let m = med(&mut e.to_vec());
    let mad = med(&mut e.iter().map(|x| (x - m).abs()).collect());
    let se = 1.2533 * 1.4826 * mad / (n as f64).sqrt();
    let u_t = 2.0 * se + u;
    if u_t > 15.0 {
        "uncertain"
    } else if m.abs() <= u_t.max(10.0) {
        "centered"
    } else if m < 0.0 {
        "early"
    } else {
        "late"
    }
}

#[test]
fn t10_an_estimate_never_claims_and_a_loop_follows_the_rule() {
    let mut rng = Rng(10);
    let mut seen = std::collections::BTreeMap::new();
    for _ in 0..10_000 {
        let n = (rng.next() % 64) as u32;
        let m = rng.range(-80.0, 80.0);
        let s = rng.range(0.0, 60.0);
        let v: Vec<PulseVerdict> = (0..n).map(|k| hit(k, m + s * rng.normal())).collect();

        let est = TimingRef::estimate(
            rng.range(-50.0, 300.0),
            rng.range(0.0, 15.0),
            1 + (rng.next() % 40) as u32,
        );
        assert!(est.judges_timing());
        for t in [est, TimingRef::none()] {
            let s = RoundStats::from_verdicts(&v, &t);
            assert_eq!(s.tendency, Tendency::NotClaimed(NoClaim::EstimateOnly));
            assert!(s.improvements(&s).is_empty());
        }

        let lp = TimingRef::loopback(rng.range(0.0, 200.0), rng.range(0.0, 4.0));
        let e: Vec<f64> = v.iter().map(|p| f64::from(p.dt_ms.unwrap())).collect();
        let want = rule(&e, lp.center_uncertainty_ms());
        let got = match tendency(&v, &lp) {
            Tendency::NotClaimed(NoClaim::TooFew) => "too few",
            Tendency::NotClaimed(NoClaim::Uncertain) => "uncertain",
            Tendency::Centered => "centered",
            Tendency::Early(x) => {
                assert!(x < 0.0);
                "early"
            }
            Tendency::Late(x) => {
                assert!(x > 0.0);
                "late"
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(got, want, "n {n} m {m} s {s}");
        *seen.entry(got).or_insert(0u32) += 1;
    }
    assert_eq!(seen.len(), 5, "{seen:?}");
    assert!(seen.values().all(|&c| c >= 50), "{seen:?}");
}

#[test]
fn only_final_timed_verdicts_enter_the_tendency() {
    let t = loop_ref();
    let clean = two_point(16, -30.0, 4.0);
    let want = RoundStats::from_verdicts(&clean, &t);
    let mut v = clean.clone();
    // none of these is a timed final verdict
    v.push(PulseVerdict {
        fin: false,
        ..hit(100, 240.0)
    });
    v.push(not_heard(101));
    v.push(void(102, Why::Glitch));
    v.push(void(103, Why::Disputed));
    v.push(void(104, Why::Ghost));
    v.push(PulseVerdict {
        timing: Timing::NotJudged(Why::GateLocked),
        dt_ms: None,
        ..hit(105, 0.0)
    });
    let got = RoundStats::from_verdicts(&v, &t);
    assert_eq!(got.tendency, want.tendency);
    assert_eq!(got.median_ms, want.median_ms);
    assert_eq!(got.spread_ms, want.spread_ms);
    assert_eq!(got.timed, 16);
    assert!(matches!(got.tendency, Tendency::Early(_)));
}

#[test]
fn a_locked_tendency_gate_hides_the_tendency() {
    let t = loop_ref();
    let s = RoundStats::from_verdicts(&two_point(16, -30.0, 4.0), &t);
    assert_eq!(s.gated(&GateTable::builtin()), s);
    let mut gates = GateTable::builtin();
    gates.set(
        VerdictType::Tendency,
        GateEntry {
            state: GateState::Locked,
            rule: GateRule::default(),
            evidence: GateEvidence::default(),
            reason: "test".into(),
        },
    );
    let g = s.gated(&gates);
    assert_eq!(g.tendency, Tendency::NotClaimed(NoClaim::GateLocked));
    assert_eq!(g.median_ms, s.median_ms);
}

// ---------------------------------------------------------------------------------------
// Counts

#[test]
fn counts_and_shares() {
    let v = vec![
        timed(0, 3.0, Timing::Center),
        timed(1, -8.0, Timing::Center),
        timed(2, 20.0, Timing::Center),
        timed(3, 60.0, Timing::InWindow),
        timed(4, -90.0, Timing::InWindow),
        timed(5, 200.0, Timing::Outside),
        not_heard(6),
        not_heard(7),
        void(8, Why::Glitch),
        void(9, Why::Disputed),
        void(10, Why::Ghost),
        PulseVerdict {
            fin: false,
            ..hit(11, 0.0)
        },
        PulseVerdict {
            timing: Timing::NotJudged(Why::GateLocked),
            dt_ms: None,
            ..hit(12, 0.0)
        },
    ];
    let s = RoundStats::from_verdicts(&v, &loop_ref());
    assert_eq!(s.events, 11, "the ghost and the provisional are not events");
    assert_eq!(
        (s.heard, s.not_heard, s.not_judged, s.disputed),
        (7, 2, 1, 1)
    );
    assert_eq!((s.center, s.in_window, s.outside, s.timed), (3, 2, 1, 6));
    assert!(close(s.heard_share().unwrap(), 7.0 / 9.0));
    assert!(close(s.in_window_share().unwrap(), 5.0 / 6.0));
    assert!(close(s.median_ms.unwrap(), 11.5));
    assert_eq!(s.tendency, Tendency::NotClaimed(NoClaim::TooFew));
    assert_eq!(s.p90_abs_ms, None);
    assert!(!s.constancy_focus);

    let empty = RoundStats::from_verdicts(&[], &loop_ref());
    assert_eq!((empty.events, empty.timed), (0, 0));
    assert_eq!(
        (empty.median_ms, empty.spread_ms, empty.uncertainty_ms),
        (None, None, None)
    );
    assert_eq!((empty.heard_share(), empty.in_window_share()), (None, None));
}

// ---------------------------------------------------------------------------------------
// Tolerance

#[test]
fn tolerance_is_the_step_of_the_90th_percentile() {
    // 20 strums: the 18th smallest |e| decides
    let round = |tail: f64, tails: usize| {
        let mut e = vec![15.0; 20 - tails];
        e.extend(std::iter::repeat_n(tail, tails));
        // signs do not matter
        hits(
            &e.iter()
                .enumerate()
                .map(|(k, x)| if k % 2 == 0 { *x } else { -*x })
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(tolerance_suggestion(&round(100.0, 2)), Some(Level::Stage));
    assert_eq!(tolerance_suggestion(&round(70.0, 3)), Some(Level::Band));
    assert_eq!(
        tolerance_suggestion(&round(110.0, 3)),
        Some(Level::Apprentice)
    );
    assert_eq!(tolerance_suggestion(&round(130.0, 3)), None);
    // the edges of the steps
    for (p90, want) in [
        (50.0, Some(Level::Stage)),
        (50.5, Some(Level::Band)),
        (80.0, Some(Level::Band)),
        (80.5, Some(Level::Apprentice)),
        (120.0, Some(Level::Apprentice)),
        (120.5, None),
    ] {
        let v = round(p90, 3);
        assert_eq!(tolerance_suggestion(&v), want, "{p90}");
        let s = RoundStats::from_verdicts(&v, &loop_ref());
        assert_eq!(s.p90_abs_ms, Some(p90));
        assert_eq!(s.constancy_focus, want.is_none(), "{p90}");
    }
    // too few to say
    assert_eq!(tolerance_suggestion(&hits(&[5.0; 11])), None);
    assert_eq!(tolerance_suggestion(&hits(&[5.0; 12])), Some(Level::Stage));
    assert!(!RoundStats::from_verdicts(&hits(&[5.0; 11]), &loop_ref()).constancy_focus);

    // An attack that was not heard is beyond every window. 18 tight strums and 3 misses:
    // the 90th percentile of 21 is the 19th, a miss. Leaving the misses out would say
    // "stage" to a player who loses one beat in seven.
    let mut v = hits(&[5.0; 18]);
    v.extend((0..3).map(|k| not_heard(100 + k)));
    assert_eq!(tolerance_suggestion(&v), None);
    let s = RoundStats::from_verdicts(&v, &loop_ref());
    assert!(s.constancy_focus);
    assert_eq!(s.p90_abs_ms, None);
    // two misses in twenty are within the tenth
    v.pop();
    assert_eq!(tolerance_suggestion(&v), Some(Level::Stage));

    // what was not judged is neither tight nor loose
    let mut v = round(70.0, 3);
    v.push(void(200, Why::Glitch));
    v.push(void(201, Why::Disputed));
    v.push(PulseVerdict {
        fin: false,
        ..hit(202, 240.0)
    });
    assert_eq!(tolerance_suggestion(&v), Some(Level::Band));
}

// ---------------------------------------------------------------------------------------
// Improvements

#[test]
fn improvements_say_only_what_got_better() {
    let t = loop_ref();
    let stats = |v: &[PulseVerdict]| RoundStats::from_verdicts(v, &t);

    // steadier
    let wide = stats(&two_point(16, 0.0, 30.0));
    let tight = stats(&two_point(16, 0.0, 20.0));
    let got = tight.improvements(&wide);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(matches!(got[0], Improvement::Steadier { from_ms, to_ms }
            if close(from_ms, 44.478) && close(to_ms, 29.652)));
    assert!(wide.improvements(&tight).is_empty(), "worse is never said");
    assert!(tight.improvements(&tight).is_empty());
    // a hair is not an improvement
    assert!(
        stats(&two_point(16, 0.0, 29.0))
            .improvements(&wide)
            .is_empty()
    );
    // nor is a comparison with a round that has too few strums
    assert!(
        stats(&two_point(6, 0.0, 5.0))
            .improvements(&wide)
            .is_empty()
    );

    // more heard
    let round = |heard: u32, missed: u32| {
        let mut v = hits(&vec![10.0; heard as usize]);
        v.extend((0..missed).map(|k| not_heard(500 + k)));
        stats(&v)
    };
    let got = round(14, 2).improvements(&round(10, 6));
    assert!(
        matches!(got[..], [Improvement::MoreHeard { from, to }]
            if close(from, 0.625) && close(to, 0.875)),
        "{got:?}"
    );
    assert!(round(10, 6).improvements(&round(14, 2)).is_empty());

    // more in time
    let round = |inside: usize, outside: usize| {
        let mut v = hits(&vec![10.0; inside]);
        v.extend((0..outside).map(|k| timed(600 + k as u32, 10.0, Timing::Outside)));
        stats(&v)
    };
    let got = round(12, 4).improvements(&round(8, 8));
    assert!(
        matches!(got[..], [Improvement::MoreInWindow { from, to }]
            if close(from, 0.5) && close(to, 0.75)),
        "{got:?}"
    );

    // closer to the click: only between rounds that both claim a tendency
    let early = stats(&two_point(16, -40.0, 5.0));
    assert!(matches!(early.tendency, Tendency::Early(_)));
    let nearer = stats(&two_point(16, -20.0, 5.0));
    let got = nearer.improvements(&early);
    assert!(
        matches!(got[..], [Improvement::CloserToCenter { from_ms, to_ms }]
            if close(from_ms, -40.0) && close(to_ms, -20.0)),
        "{got:?}"
    );
    let centered = stats(&two_point(16, -5.0, 5.0));
    assert_eq!(centered.tendency, Tendency::Centered);
    assert!(matches!(
        centered.improvements(&early)[..],
        [Improvement::CloserToCenter { .. }]
    ));
    // within the scatter of two medians: not said
    assert!(
        stats(&two_point(16, -38.0, 5.0))
            .improvements(&early)
            .is_empty()
    );
    // from the other side, farther: not said
    assert!(
        stats(&two_point(16, 45.0, 5.0))
            .improvements(&early)
            .is_empty()
    );
    // already centered: nothing to move toward
    assert!(centered.improvements(&centered).is_empty());
    assert!(
        stats(&two_point(16, -2.0, 5.0))
            .improvements(&centered)
            .is_empty()
    );
    // without a loop the same numbers say nothing about the center
    let est = TimingRef::estimate(100.0, 5.0, 16);
    let a = RoundStats::from_verdicts(&two_point(16, -40.0, 5.0), &est);
    let b = RoundStats::from_verdicts(&two_point(16, -20.0, 5.0), &est);
    assert!(b.improvements(&a).is_empty());

    // all at once, most basic first
    let mut before = two_point(16, -40.0, 12.0);
    before.extend((0..8).map(|k| timed(700 + k, -40.0, Timing::Outside)));
    before.extend((0..8).map(|k| not_heard(800 + k)));
    let mut after = two_point(24, -15.0, 6.0);
    after.extend((0..2).map(|k| not_heard(800 + k)));
    let got = stats(&after).improvements(&stats(&before));
    assert!(
        matches!(
            got[..],
            [
                Improvement::MoreHeard { .. },
                Improvement::MoreInWindow { .. },
                Improvement::Steadier { .. },
                Improvement::CloserToCenter { .. }
            ]
        ),
        "{got:?}"
    );
}

// ---------------------------------------------------------------------------------------
// VerdictLog

#[test]
fn the_log_keeps_one_final_verdict_per_event() {
    let mut log = VerdictLog::new();
    log.push(&JudgeOut::Flash {
        id: Some(0),
        dt_ms: Some(4.0),
    });
    log.push(&JudgeOut::Verdict(PulseVerdict {
        fin: false,
        ..hit(0, 4.0)
    }));
    assert!(
        log.verdicts().is_empty(),
        "provisional verdicts are not kept"
    );
    log.push(&JudgeOut::Verdict(hit(0, 4.0)));
    log.push(&JudgeOut::Verdict(not_heard(1)));
    log.push(&JudgeOut::Verdict(hit(2, -70.0)));
    log.push(&JudgeOut::Extra { t_in: 123.0 });
    log.push(&JudgeOut::Extra { t_in: 456.0 });
    assert_eq!(log.verdicts().len(), 3);
    assert_eq!(log.extras(), 2);

    // a withdrawal turns the verdict into "not judged": no hit, no miss
    log.push(&JudgeOut::Withdrawn {
        id: 1,
        why: Why::Glitch,
    });
    log.push(&JudgeOut::Withdrawn {
        id: 2,
        why: Why::Disputed,
    });
    log.push(&JudgeOut::Withdrawn {
        id: 77,
        why: Why::Glitch,
    });
    let v = log.verdicts();
    assert_eq!(v.len(), 3);
    assert_eq!(v[0], hit(0, 4.0));
    assert_eq!(v[1].attack, Attack::NotJudged(Why::Glitch));
    assert_eq!(v[2].attack, Attack::NotJudged(Why::Disputed));
    assert_eq!(v[2].timing, Timing::NotJudged(Why::Disputed));
    assert_eq!(v[2].dt_ms, None);
    assert_eq!(
        v[2].onset_in,
        hit(2, 0.0).onset_in,
        "what was heard stays on record"
    );
    let s = log.stats(&loop_ref());
    assert_eq!(
        (s.events, s.heard, s.not_heard, s.not_judged, s.disputed),
        (3, 1, 0, 1, 1)
    );
    assert_eq!(s.timed, 1);

    log.clear();
    assert!(log.verdicts().is_empty());
    assert_eq!(log.extras(), 0);
    assert_eq!(log, VerdictLog::new());
}

// ---------------------------------------------------------------------------------------
// The two worlds of plan §2.1, at the judge

/// A round of 32 quarter notes at 80 BPM by a player `tendency_ms` off the click with
/// scatter `sigma_ms`, in a system whose loop is `latency_ms`. Returns the log.
fn world(latency_ms: f64, tendency_ms: f64, sigma_ms: f64, timing: TimingRef) -> VerdictLog {
    const FS: f64 = 48_000.0;
    let gap = 750.0;
    let fr = |ms: f64| 500_000.0 + ms * FS / 1000.0;
    let mut j = PulseJudge::new(
        JudgeConfig::new(FS, Level::Apprentice),
        timing,
        &GateTable::builtin(),
    );
    let mut rng = Rng(42);
    let mut log = VerdictLog::new();
    let mut out = Vec::new();
    let mut push = |j: &mut PulseJudge, i: JudgeInput| {
        j.push(&i, &mut out);
        for o in out.drain(..) {
            log.push(&o);
        }
    };
    push(
        &mut j,
        JudgeInput::Progress {
            in_now: fr(-1000.0),
            locked: true,
        },
    );
    for k in 0..32u32 {
        let g = f64::from(k) * gap;
        push(
            &mut j,
            JudgeInput::Expect {
                id: k,
                g_in: fr(g),
                kind: ExpectKind::Attack,
                prev_gap_s: gap / 1000.0,
                next_gap_s: gap / 1000.0,
                pos_in_bar: (k % 4 * 2) as u8,
            },
        );
        // what the microphone hears: the click's loop, the player's habit, the scatter
        let t = g + latency_ms + tendency_ms + sigma_ms * rng.normal();
        push(
            &mut j,
            JudgeInput::Strum {
                t_in: fr(t),
                judged: true,
            },
        );
        push(
            &mut j,
            JudgeInput::Progress {
                in_now: fr(g + 700.0),
                locked: true,
            },
        );
    }
    push(
        &mut j,
        JudgeInput::Progress {
            in_now: fr(40_000.0),
            locked: true,
        },
    );
    log
}

#[test]
fn two_worlds_are_told_apart_only_with_a_loop() {
    // World A: loop 87.25 ms, a player 30 ms early. World B: loop 57.25 ms, a player on
    // the click. The microphone hears the same thing in both (the numbers are exact in
    // binary, so "the same" is bit for bit).
    let (la, lb) = (87.25, 57.25);
    let sigma = 9.0;

    // With the loop measured the game tells them apart, and shows the habit: it does not
    // subtract it.
    let a = world(la, -30.0, sigma, TimingRef::loopback(la, 0.3));
    let b = world(lb, 0.0, sigma, TimingRef::loopback(lb, 0.3));
    let sa = a.stats(&TimingRef::loopback(la, 0.3));
    let sb = b.stats(&TimingRef::loopback(lb, 0.3));
    assert_eq!((sa.timed, sb.timed), (32, 32));
    match sa.tendency {
        Tendency::Early(m) => assert!((m + 30.0).abs() <= 3.0, "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(sb.tendency, Tendency::Centered);
    assert!(sb.median_ms.unwrap().abs() <= 3.0);
    // every strum of A carries its own 30 ms
    let mean = a
        .verdicts()
        .iter()
        .map(|v| f64::from(v.dt_ms.unwrap()))
        .sum::<f64>()
        / 32.0;
    assert!((mean + 30.0).abs() <= 3.0, "{mean}");

    // Without a loop the calibration is the median of the player's own strums: 57.25 ms in
    // both worlds. The verdicts are the same and nothing is said about a tendency.
    let est = TimingRef::estimate(57.25, 6.0, 16);
    let a = world(la, -30.0, sigma, est);
    let b = world(lb, 0.0, sigma, est);
    assert_eq!(a, b);
    for w in [&a, &b] {
        let s = w.stats(&est);
        assert_eq!(s.timed, 32);
        assert_eq!(s.tendency, Tendency::NotClaimed(NoClaim::EstimateOnly));
        assert!(s.median_ms.unwrap().abs() <= 3.0);
        assert_eq!(s.center, 0, "no \"center\" without a loop");
    }

    // The old way (subtract loop and habit together, then speak as if the loop had been
    // measured) is what this must never do: it would call player A centered.
    let old = world(la, -30.0, sigma, TimingRef::loopback(la - 30.0, 0.3));
    assert_eq!(
        old.stats(&TimingRef::loopback(la - 30.0, 0.3)).tendency,
        Tendency::Centered,
        "the control: a habit hidden in the offset disappears from the verdicts"
    );
}
