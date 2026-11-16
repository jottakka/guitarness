//! Report of the onset detector over the grid, for a few configurations (run by hand).
//!
//! `cargo test --release -p tonica-sim --test sweep -- --ignored --nocapture`
//! `GRID=full` runs the three seeds; `CFG=all` adds the variants.

use tonica_dsp::OnsetConfig;
use tonica_sim::timing::{Score, detect, make_take, regressions, score, standard_grid, summarize};

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * q).round() as usize]
}

#[test]
#[ignore]
fn sweep() {
    let full = std::env::var("GRID").as_deref() == Ok("full");
    let conds = standard_grid(!full);
    let regs = regressions();
    let mut cfgs: Vec<(&str, OnsetConfig)> = vec![("default", OnsetConfig::default())];
    if std::env::var("CFG").as_deref() == Ok("all") {
        let d = OnsetConfig::default;
        cfgs.push((
            "rise1.5",
            OnsetConfig {
                rise_min_db: 1.5,
                ..d()
            },
        ));
        cfgs.push((
            "rise2.5",
            OnsetConfig {
                rise_min_db: 2.5,
                ..d()
            },
        ));
        cfgs.push((
            "rise3",
            OnsetConfig {
                rise_min_db: 3.0,
                ..d()
            },
        ));
        cfgs.push((
            "heard3",
            OnsetConfig {
                heard_db: 3.0,
                ..d()
            },
        ));
        cfgs.push((
            "heard6",
            OnsetConfig {
                heard_db: 6.0,
                ..d()
            },
        ));
        cfgs.push((
            "no-mask",
            OnsetConfig {
                mask_drop_db: 200.0,
                ..d()
            },
        ));
        cfgs.push((
            "no-noise",
            OnsetConfig {
                noise_k: 0.0,
                ..d()
            },
        ));
        cfgs.push((
            "one-band",
            OnsetConfig {
                band_hz: [2000.0; 4],
                ..d()
            },
        ));
    }
    for (name, cfg) in cfgs {
        let mut all = Score::default();
        // the last entry takes the cells without a tempo in their name (44.1 kHz, 90 BPM)
        let tempos = ["_60x2", "_100x2", "_120x4", "_180x2", "_sr44k"];
        let mut by_tempo = [(0usize, 0usize); 5];
        // Heard notices that belong to no strum; second notices for one strum; audio length
        let (mut stray, mut twice, mut secs) = (0usize, 0usize, 0.0f64);
        for c in &conds {
            let tk = make_take(c);
            let det = detect(&tk, &cfg);
            let mut sc = score(&tk, &det, 60.0);
            for f in sc.faults.iter_mut() {
                *f = format!("{} {f}", c.name);
            }
            let k = tempos.iter().position(|t| c.name.contains(t)).unwrap_or(4);
            by_tempo[k].0 += sc.hits;
            by_tempo[k].1 += sc.n;
            let tol = 0.060 * tk.sr;
            stray += det
                .heard
                .iter()
                .filter(|h| !tk.truth.iter().any(|t| (h.0 - t).abs() <= tol))
                .count();
            for (i, t) in tk.truth.iter().enumerate() {
                let near: Vec<&(f64, f64)> = det
                    .heard
                    .iter()
                    .filter(|h| (h.0 - t).abs() <= tol)
                    .collect();
                twice += near.len().saturating_sub(1);
                if near.len() > 1 && std::env::var("VERBOSE").is_ok() {
                    let ms = |v: f64| v / tk.sr * 1000.0;
                    let l: Vec<String> = near
                        .iter()
                        .map(|h| format!("t{:+.1} at{:+.1}", ms(h.0 - t), ms(h.1 - t)))
                        .collect();
                    let nx = tk.truth.get(i + 1).map(|n| ms(n - t)).unwrap_or(f64::NAN);
                    println!(
                        "  twice {} #{i} spread {:.0} ms, next strum +{nx:.0} ms: {}",
                        c.name,
                        tk.spread[i] * 1000.0,
                        l.join(" | ")
                    );
                }
            }
            secs += tk.audio.len() as f64 / tk.sr;
            all.add(&sc);
        }
        let s = summarize(&all);
        let mut h = all.heard_ms.clone();
        // BUDGET=<ms> moves the limit (to compare detectors whose `at` differs by a known delay)
        let budget = std::env::var("BUDGET")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(tonica_sim::timing::HEARD_BUDGET_MS);
        let within = h.iter().filter(|v| **v <= budget).count() as f64 / all.n as f64;
        println!(
            "{name:10} n={} rec={:.4} ({} missed) extra={:.4} ({}) bias={:+.2} σ={:.2} p95={:.2} max={:.1}",
            s.n,
            s.recall,
            all.n - all.hits,
            s.extra_rate,
            all.extras,
            s.bias_ms,
            s.sigma_ms,
            s.p95_abs_ms,
            s.max_abs_ms
        );
        let t: Vec<String> = tempos
            .iter()
            .zip(&by_tempo)
            .map(|(t, (h, n))| format!("{}={:.4}", &t[1..], *h as f64 / *n as f64))
            .collect();
        println!("           recall by tempo: {}", t.join(" "));
        println!(
            "           Heard: {:.4} of strums within {budget} ms; latency p50={:.1} p95={:.1} p99={:.1} max={:.1} ms; stray {}, twice {} in {:.0} s; without a strum {}, contract breaches {}",
            within,
            pct(&mut h, 0.5),
            pct(&mut h, 0.95),
            pct(&mut h, 0.99),
            pct(&mut h, 1.0),
            stray,
            twice,
            secs,
            all.unpaired,
            all.faults.len()
        );
        if std::env::var("VERBOSE").is_ok() {
            for f in &all.faults {
                println!("  breach {f}");
            }
        }
        let reg: Vec<String> = regs
            .iter()
            .map(|r| {
                let d = detect(&r.take, &cfg);
                format!(
                    "{}={}/{} (heard {}, breaches {})",
                    r.name,
                    d.strums.len(),
                    r.expect,
                    d.heard.len(),
                    tonica_sim::timing::stream_faults(&d.events, r.take.sr).len()
                )
            })
            .collect();
        println!("           {}", reg.join(", "));
    }
}
