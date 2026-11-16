use tonica_dsp::OnsetConfig;
use tonica_sim::timing::{Cond, detect, make_take};

#[test]
#[ignore]
fn diag() {
    let mut c = Cond::base("d", 5);
    c.snr_db = 40.0;
    c.bpm = 120.0;
    c.per_beat = 4;
    let tk = make_take(&c);
    let d = detect(&tk, &OnsetConfig::default());
    for (t, at) in &d.heard {
        let _ = (t, at);
    }
    println!(
        "rejected={} detected={} truth={}",
        d.rejected,
        d.strums.len(),
        tk.truth.len()
    );
    let ms = |s: f64| s / tk.sr * 1000.0;
    for (i, t) in tk.truth.iter().enumerate() {
        let near = d
            .strums
            .iter()
            .map(|x| ms(x - t))
            .filter(|e| e.abs() < 60.0)
            .fold(f64::NAN, |a, b| {
                if a.is_nan() || b.abs() < a.abs() {
                    b
                } else {
                    a
                }
            });
        println!(
            "{i:2} t={:8.1}ms spread={:5.1} err={near:+.2}",
            ms(*t),
            tk.spread[i] * 1000.0
        );
    }
}

/// Writes takes, stems and the regressions as raw f32 for offline exploration.
/// `DUMP_DIR=/path cargo test -p tonica-sim --test diag dump -- --ignored`
#[test]
#[ignore]
fn dump() {
    use std::io::Write;
    use tonica_sim::timing::{make_take_stems, regressions, standard_grid};
    let dir = std::env::var("DUMP_DIR").expect("DUMP_DIR");
    let raw = |name: &str, x: &[f32]| {
        let mut f = std::fs::File::create(format!("{dir}/{name}")).unwrap();
        let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
        f.write_all(&bytes).unwrap();
    };
    let cfg = OnsetConfig::default();
    let mut index = String::new();
    for c in standard_grid(true) {
        let (tk, st) = make_take_stems(&c);
        let d = detect(&tk, &cfg);
        raw(&format!("{}.audio.f32", c.name), &tk.audio);
        raw(&format!("{}.dry.f32", c.name), &st.dry);
        raw(&format!("{}.fresh.f32", c.name), &st.fresh);
        let t: Vec<String> = tk.truth.iter().map(|v| format!("{v:.1}")).collect();
        let sp: Vec<String> = tk
            .spread
            .iter()
            .map(|v| format!("{:.2}", v * 1000.0))
            .collect();
        let det: Vec<String> = d.strums.iter().map(|v| format!("{v:.1}")).collect();
        let info: Vec<String> = st
            .specs
            .iter()
            .map(|s| {
                format!(
                    "{}{}{}:{:.1}",
                    if s.dir == tonica_sim::strum::Dir::Down {
                        "D"
                    } else {
                        "U"
                    },
                    if s.muted { "m" } else { "" },
                    s.strings.iter().filter(|b| **b).count(),
                    20.0 * s.gain.log10()
                )
            })
            .collect();
        index += &format!(
            "take {} {} | {} | {} | {} | {}\n",
            c.name,
            tk.sr,
            t.join(" "),
            sp.join(" "),
            det.join(" "),
            info.join(" ")
        );
    }
    for r in regressions() {
        let d = detect(&r.take, &cfg);
        raw(&format!("reg_{}.audio.f32", r.name), &r.take.audio);
        let t: Vec<String> = r.take.truth.iter().map(|v| format!("{v:.1}")).collect();
        let det: Vec<String> = d.strums.iter().map(|v| format!("{v:.1}")).collect();
        index += &format!(
            "reg {} {} | {} | {} | {}\n",
            r.name,
            r.take.sr,
            t.join(" "),
            r.expect,
            det.join(" ")
        );
    }
    std::fs::write(format!("{dir}/index.txt"), index).unwrap();
}

/// Lists every miss and extra of a configuration over the quick grid (`GRID=full`: all three
/// seeds), with context. `MODE=flux` drops the energy test (what the detection branch alone
/// finds).
#[test]
#[ignore]
fn misses() {
    use tonica_sim::timing::standard_grid;
    let mut cfg = OnsetConfig::default();
    if std::env::var("MODE").as_deref() == Ok("flux") {
        cfg.rise_min_db = -200.0;
    }
    let (mut n_miss, mut n_extra) = (0, 0);
    for c in standard_grid(std::env::var("GRID").as_deref() != Ok("full")) {
        let tk = make_take(&c);
        let d = detect(&tk, &cfg);
        let ms = |s: f64| s / tk.sr * 1000.0;
        let mut used = vec![false; d.strums.len()];
        for (i, &t) in tk.truth.iter().enumerate() {
            let best = d
                .strums
                .iter()
                .enumerate()
                .filter(|(k, x)| !used[*k] && ms((**x - t).abs()) <= 60.0)
                .min_by(|a, b| (a.1 - t).abs().total_cmp(&(b.1 - t).abs()));
            match best {
                Some((k, _)) => used[k] = true,
                None => {
                    n_miss += 1;
                    let prev = if i > 0 {
                        ms(t - tk.truth[i - 1]) - tk.spread[i - 1] * 1000.0
                    } else {
                        f64::NAN
                    };
                    let next = if i + 1 < tk.truth.len() {
                        ms(tk.truth[i + 1] - t) - tk.spread[i] * 1000.0
                    } else {
                        f64::NAN
                    };
                    let near: Vec<String> = d
                        .strums
                        .iter()
                        .map(|x| ms(x - t))
                        .filter(|e| e.abs() < 200.0)
                        .map(|e| format!("{e:+.0}"))
                        .collect();
                    println!(
                        "MISS  {:36} #{i:2} t={:7.1} spread={:4.1} gap_prev={prev:5.1} gap_next={next:5.1} det near: {}",
                        c.name,
                        ms(t),
                        tk.spread[i] * 1000.0,
                        near.join(" ")
                    );
                }
            }
        }
        for (k, x) in d.strums.iter().enumerate() {
            if !used[k] {
                n_extra += 1;
                let near: Vec<String> = tk
                    .truth
                    .iter()
                    .map(|t| ms(x - t))
                    .filter(|e| e.abs() < 200.0)
                    .map(|e| format!("{e:+.0}"))
                    .collect();
                println!(
                    "EXTRA {:36} t={:7.1} truth near (det-truth): {}",
                    c.name,
                    ms(*x),
                    near.join(" ")
                );
            }
        }
    }
    println!("misses={n_miss} extras={n_extra}");
}

/// Cost of the onset path per 256-sample block (run with `--release`).
#[test]
#[ignore]
fn cost() {
    use tonica_dsp::OnsetDetector;
    let mut c = Cond::base("cost", 3);
    c.bpm = 120.0;
    c.per_beat = 4;
    c.events = 64;
    c.snr_db = 30.0;
    c.reverb_wet = 0.25;
    let tk = make_take(&c);
    // Per block, the fastest of several passes: what the block costs without the scheduler.
    // The slowest single measurement is printed too (it includes the machine's other work).
    let blocks = tk.audio.len().div_ceil(256);
    let mut least = vec![f64::INFINITY; blocks];
    let mut worst = 0.0f64;
    for _ in 0..9 {
        let mut d = OnsetDetector::new(tk.sr, OnsetConfig::default());
        let mut n_ev = 0usize;
        for (k, b) in tk.audio.chunks(256).enumerate() {
            let t0 = std::time::Instant::now();
            d.process(std::hint::black_box(b), |_| n_ev += 1);
            let us = t0.elapsed().as_secs_f64() * 1e6;
            least[k] = least[k].min(us);
            worst = worst.max(us);
        }
        std::hint::black_box(n_ev);
    }
    let mean = least.iter().sum::<f64>() / blocks as f64;
    least.sort_by(f64::total_cmp);
    println!(
        "onset cost per 256-sample block: mean {:.2} µs, p99.9 {:.2} µs, max {:.2} µs (slowest single pass {:.2} µs)",
        mean,
        least[(blocks as f64 * 0.999) as usize],
        least[blocks - 1],
        worst
    );
}

/// Trace one grid cell: `CELL=<name> ONSET_DBG=1 cargo test -p tonica-sim --test diag cell -- --ignored --nocapture`
/// (a debug build: the trace of the detector is not compiled into release builds).
#[test]
#[ignore]
fn cell() {
    use tonica_sim::timing::standard_grid;
    let name = std::env::var("CELL").expect("CELL");
    let c = standard_grid(true)
        .into_iter()
        .find(|c| c.name == name)
        .expect("no such cell");
    let tk = make_take(&c);
    let ms = |s: f64| s / tk.sr * 1000.0;
    for (i, t) in tk.truth.iter().enumerate() {
        println!(
            "TRUTH {i:2} t={:8.1} ms = {:.0} analysis samples, spread {:.1} ms",
            ms(*t),
            t / 2.0,
            tk.spread[i] * 1000.0
        );
    }
    let d = detect(&tk, &OnsetConfig::default());
    for s in &d.strums {
        println!("STRUM t={:8.1} ms", ms(*s));
    }
}

/// Where a finger landing on a ringing string starts to count as a strum: extra strums and
/// extra `Heard` notices out of 7 landings (the last three within 50 ms), by level, fingertip
/// softness and room noise.
#[test]
#[ignore]
fn hammer_limit() {
    use tonica_sim::timing::{Hammer, hammer_take};
    let cfg = OnsetConfig::default();
    for snr in [40.0, 30.0, 20.0] {
        println!(
            "SNR {snr:.0} dB: extra strums / extra Heard, by level (columns) and low-pass (rows)"
        );
        let levels = [-6.0, -12.0, -18.0, -24.0, -30.0, -36.0];
        let head: Vec<String> = levels.iter().map(|l| format!("{l:6.0}")).collect();
        println!("  lp_hz   {}", head.join(" "));
        for lp in [48000.0, 4000.0, 2500.0, 1500.0, 800.0] {
            let row: Vec<String> = levels
                .iter()
                .map(|&level_db| {
                    let d = detect(
                        &hammer_take(
                            Hammer {
                                level_db,
                                lp_hz: lp,
                            },
                            snr,
                        ),
                        &cfg,
                    );
                    format!(
                        "{:3}/{:<2}",
                        d.strums.len() as i64 - 1,
                        d.heard.len() as i64 - 1
                    )
                })
                .collect();
            println!("  {lp:6.0}  {}", row.join(" "));
        }
    }
}

/// The grid through a noise gate (exact zeros whenever nothing louder than the room is heard):
/// the noise floor of the detector then never sees the room alone.
#[test]
#[ignore]
fn gated() {
    use tonica_sim::timing::{Score, score, standard_grid, summarize};
    let cfg = OnsetConfig::default();
    for gate in [false, true] {
        let mut all = Score::default();
        let (mut stray, mut zeros, mut total) = (0usize, 0usize, 0usize);
        for c in standard_grid(true) {
            let mut tk = make_take(&c);
            if gate {
                // opens 12 dB above the room noise, holds 150 ms, 1 ms / 20 ms ramps
                let thr = 0.1 * 10f64.powf(-c.snr_db / 20.0) * 4.0;
                let (mut env, mut hold, mut g) = (0.0f64, 0usize, 0.0f64);
                let dec = (-1.0 / (0.005 * tk.sr)).exp();
                for v in tk.audio.iter_mut() {
                    env = (v.abs() as f64).max(env * dec);
                    if env > thr {
                        hold = (0.150 * tk.sr) as usize;
                    } else {
                        hold = hold.saturating_sub(1);
                    }
                    let target = if hold > 0 { 1.0 } else { 0.0 };
                    let step = if target > g {
                        1.0 / (0.001 * tk.sr)
                    } else {
                        1.0 / (0.020 * tk.sr)
                    };
                    g = if target > g {
                        (g + step).min(1.0)
                    } else {
                        (g - step).max(0.0)
                    };
                    *v = (*v as f64 * g) as f32;
                }
                zeros += tk.audio.iter().filter(|v| **v == 0.0).count();
                total += tk.audio.len();
            }
            let d = detect(&tk, &cfg);
            let tol = 0.060 * tk.sr;
            stray += d
                .heard
                .iter()
                .filter(|h| !tk.truth.iter().any(|t| (h.0 - t).abs() <= tol))
                .count();
            let sc = score(&tk, &d, 60.0);
            if std::env::var("VERBOSE").is_ok() {
                let ms = |v: f64| v / tk.sr * 1000.0;
                for x in &d.strums {
                    let e = tk
                        .truth
                        .iter()
                        .map(|t| ms(x - t))
                        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
                        .unwrap();
                    if e.abs() > 5.0 {
                        println!(
                            "  gate={gate} {:36} strum at {:7.1} ms, {e:+.1} ms from the nearest truth",
                            c.name,
                            ms(*x)
                        );
                    }
                }
            }
            all.add(&sc);
        }
        let s = summarize(&all);
        println!(
            "gate={gate}: rec={:.4} ({} missed) extras={} bias={:+.2} σ={:.2} max={:.1} heard95={:.1} stray Heard={} zeros={:.0}%",
            s.recall,
            all.n - all.hits,
            all.extras,
            s.bias_ms,
            s.sigma_ms,
            s.max_abs_ms,
            s.heard_p95_ms,
            stray,
            100.0 * zeros as f64 / total.max(1) as f64
        );
    }
}

/// Margin of the out-of-band guard: the largest rise the game's clicks cause in a silent
/// input, bracketed by the lowest `heard_db` that stays silent, for a few guard levels and
/// click levels (0 dB = the level the game plays them at).
#[test]
#[ignore]
fn click_margin() {
    use tonica_sim::timing::regressions;
    let regs = regressions();
    for r in regs
        .iter()
        .filter(|r| r.name.starts_with("metronome_digital_silence"))
    {
        for guard_db in [-54.0f32, -60.0, -200.0] {
            for gain_db in [6.0f64, 0.0, -20.0, -40.0] {
                let g = 10f64.powf(gain_db / 20.0);
                let audio: Vec<f32> = r
                    .take
                    .audio
                    .iter()
                    .map(|v| (*v as f64 * g) as f32)
                    .collect();
                let tk = tonica_sim::timing::Take {
                    sr: r.take.sr,
                    audio,
                    truth: Vec::new(),
                    spread: Vec::new(),
                };
                let steps = [
                    0.1f32, 0.2, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 16.0, 32.0,
                ];
                let quiet = steps.iter().find(|&&h| {
                    let cfg = OnsetConfig {
                        guard_db,
                        heard_db: h,
                        ..OnsetConfig::default()
                    };
                    detect(&tk, &cfg).heard.is_empty()
                });
                println!(
                    "{} guard {guard_db:5.0} dB, clicks {gain_db:+.0} dB: rise below {quiet:?} dB",
                    r.name
                );
            }
        }
    }
}
