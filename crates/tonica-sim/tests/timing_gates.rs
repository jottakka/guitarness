//! Timing gates of DESIGN §1.11 on the synthetic grid, plus the no-strum regressions.

use std::sync::OnceLock;

use tonica_dsp::OnsetConfig;
use tonica_sim::timing::{
    HAMMER_FIRM, HEARD_BUDGET_MS, Score, Summary, detect, hammer_take, regressions, run_grid,
    standard_grid, stream_faults, summarize,
};

/// The quick grid, run once for all the gates.
fn grid() -> &'static (Vec<(String, Summary)>, Score) {
    static GRID: OnceLock<(Vec<(String, Summary)>, Score)> = OnceLock::new();
    GRID.get_or_init(|| run_grid(&standard_grid(true), &OnsetConfig::default()))
}

#[test]
fn onset_timing_gates() {
    let (rows, all) = grid();
    let s = summarize(all);
    for (name, r) in rows {
        println!(
            "{name:36} n={:3} rec={:.3} extra={:.3} bias={:+.2} σ={:.2} p95={:.2} max={:.1} heard95={:.1}",
            r.n,
            r.recall,
            r.extra_rate,
            r.bias_ms,
            r.sigma_ms,
            r.p95_abs_ms,
            r.max_abs_ms,
            r.heard_p95_ms
        );
    }
    println!("ALL {s:?}");
    assert!(s.recall >= 0.995, "recall {:.4}", s.recall);
    assert!(s.extra_rate <= 0.005, "extras {:.4}", s.extra_rate);
    assert!(s.bias_ms.abs() <= 2.0, "bias {:.2} ms", s.bias_ms);
    assert!(s.sigma_ms <= 4.0, "σ {:.2} ms", s.sigma_ms);
}

/// The early notice against the true attacks: it is in time for the flash, it never leaves
/// before the attack, nearly every strum gets one in time, a flash without a strum is rare,
/// and the stream keeps the contract of the two events (every `Strum` directly follows a
/// `Heard` of its own).
#[test]
fn heard_notice_gates() {
    let (_, all) = grid();
    let s = summarize(all);
    println!(
        "Heard: in time {:.4}, p95 {:.2} ms, earliest {:+.2} ms, {} without a strum, {} strums",
        s.heard_in_time, s.heard_p95_ms, s.heard_min_ms, all.unpaired, all.n
    );
    assert!(
        s.heard_p95_ms <= HEARD_BUDGET_MS,
        "Heard p95 {:.2} ms after the attack",
        s.heard_p95_ms
    );
    assert!(
        s.heard_in_time >= 0.98,
        "only {:.4} of the strums got a notice within {HEARD_BUDGET_MS} ms",
        s.heard_in_time
    );
    assert!(
        s.heard_min_ms > 0.0,
        "a notice left {:.2} ms before its attack",
        -s.heard_min_ms
    );
    assert!(
        all.unpaired as f64 <= 0.005 * all.n as f64,
        "{} notices without a strum in {} strums",
        all.unpaired,
        all.n
    );
    assert!(all.faults.is_empty(), "{:#?}", all.faults);
}

/// Nothing but a real strum gives a `Strum`, and nothing else gives a `Heard` either (no
/// false flash): room noise, the metronome (through the room, and alone in a silent input),
/// ringing, the fretting hand changing shape, sliding, bending, placing fingers, a hand
/// resting on the strings.
#[test]
fn no_strum_regressions() {
    for r in regressions() {
        let d = detect(&r.take, &OnsetConfig::default());
        assert_eq!(
            d.strums.len(),
            r.expect,
            "{}: got {:?} (rejected {})",
            r.name,
            d.strums,
            d.rejected
        );
        assert_eq!(d.heard.len(), r.expect, "{}: Heard {:?}", r.name, d.heard);
        let faults = stream_faults(&d.events, r.take.sr);
        assert!(faults.is_empty(), "{}: {faults:#?}", r.name);
    }
}

/// Known limit, not met: a firm hammer-on without the picking hand counts as a strum. It is a
/// new excitation as loud and as bright as a soft pick stroke on one string, so the evidence
/// that finds soft re-strikes finds it too. Kept here so the limit stays visible.
#[test]
#[ignore = "known limit: a firm hammer-on is a new excitation, as loud as a soft pick stroke"]
fn firm_hammer_on_is_not_a_strum() {
    let d = detect(&hammer_take(HAMMER_FIRM, 30.0), &OnsetConfig::default());
    assert_eq!(d.strums.len(), 1, "got {:?}", d.strums);
}
