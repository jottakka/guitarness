//! S4: the fake rig and the virtual player. No test opens a real audio device.

use guitarness_audio::{EngineEvent, FakeConfig};
use guitarness_session::rig::{
    ChartHit, FakeRig, HitKind, PlayerModel, PlayerOptions, TruthStrum, real_devices_allowed,
};

const FS: u32 = 48_000;

fn config(fs_in: u32, fs_out: u32, latency_ms: f64, noise: f32) -> FakeConfig {
    let mut fc = FakeConfig::new(fs_in, fs_out);
    fc.path.latency_s = latency_ms / 1.0e3;
    fc.path.noise_rms = noise;
    fc
}

/// `n` hits of `kind`, 0.6 s apart from chart time 0.
fn hits(n: u32, kind: HitKind) -> Vec<ChartHit> {
    (0..n)
        .map(|id| ChartHit {
            id,
            t_s: 0.6 * f64::from(id),
            kind,
        })
        .collect()
}

/// Output frame of chart time zero, 0.5 s from now.
fn anchor(r: &FakeRig) -> u64 {
    let t = r.truth().unwrap();
    t.out_frame_at(r.now_s() + 0.5).round() as u64
}

/// Runs to the end of the chart, collecting the engine events and the raw tap.
fn run(r: &mut FakeRig, seconds: f64) -> (Vec<EngineEvent>, Vec<f32>) {
    r.io.tap.set(true);
    let (mut events, mut tap) = (Vec::new(), Vec::new());
    let mut buf = vec![0.0f32; 4096];
    let mut left = seconds;
    while left > 0.0 {
        let step = left.min(0.1);
        r.advance(step);
        left -= step;
        while let Ok(ev) = r.io.events.pop() {
            events.push(ev);
        }
        loop {
            let n = r.io.tap.read(&mut buf);
            if n == 0 {
                break;
            }
            tap.extend_from_slice(&buf[..n]);
        }
    }
    (events, tap)
}

/// Frames of the onsets the detector called strums.
fn strums(events: &[EngineEvent]) -> Vec<f64> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Onset(n) if n.at_in.is_none() => Some(n.t_in),
            _ => None,
        })
        .collect()
}

fn mean_sd(v: &[f64]) -> (f64, f64) {
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64;
    (m, var.sqrt())
}

fn played(truth: &[TruthStrum]) -> Vec<&TruthStrum> {
    truth.iter().filter(|t| t.played).collect()
}

#[test]
fn truth_agrees_with_the_detector_within_2_ms() {
    for (fs_in, fs_out, drift_in) in [(48_000, 48_000, 0.0), (44_100, 48_000, 120.0)] {
        let mut fc = config(fs_in, fs_out, 87.3, 1.0e-3);
        fc.drift_in_ppm = drift_in;
        let mut r = FakeRig::new(fc);
        let model = PlayerModel {
            tendency_ms: 18.0,
            sigma_ms: 9.0,
            seed: 5,
            ..PlayerModel::default()
        };
        let a = anchor(&r);
        let truth = r.schedule_player(a, &hits(12, HitKind::Strum), &model);
        assert_eq!(truth.len(), 12);
        let (events, _) = run(&mut r, 9.0);
        let found = strums(&events);
        assert_eq!(found.len(), 12, "{fs_in}/{fs_out}: {found:?}");
        let mut worst = 0.0f64;
        for t in &truth {
            let onset = t.onset_in.unwrap();
            // The detector reports each strum once, near the true onset.
            let near = found
                .iter()
                .map(|g| (g - onset).abs())
                .fold(f64::INFINITY, f64::min);
            worst = worst.max(near / f64::from(fs_in) * 1.0e3);
        }
        eprintln!("{fs_in}/{fs_out}: worst detector error {worst:.3} ms");
        assert!(worst <= 2.0, "{fs_in}/{fs_out}: {worst:.3} ms");
        assert_eq!(r.dropped_strums(), 0);
        assert_eq!(r.pending_strums(), 0);
    }
}

#[test]
fn the_stated_tendency_and_spread_are_the_truth() {
    let mut r = FakeRig::new(config(FS, FS, 60.0, 0.0));
    let model = PlayerModel {
        tendency_ms: -14.0,
        sigma_ms: 11.0,
        seed: 3,
        ..PlayerModel::default()
    };
    let a = anchor(&r);
    let truth = r.schedule_player(a, &hits(40, HitKind::Strum), &model);
    let dt: Vec<f64> = truth.iter().map(|t| t.dt_ms.unwrap()).collect();
    let (m, sd) = mean_sd(&dt);
    assert!((m + 14.0).abs() < 0.2, "mean {m}");
    assert!((sd - 11.0).abs() < 0.3, "sd {sd}");
    // g_k is the click as captured, the arrival is the latency later.
    for t in &truth {
        let lat = (t.arrival_in - t.g_in) / f64::from(FS) * 1.0e3;
        assert!((lat - 60.0).abs() < 0.01, "latency {lat}");
    }
}

#[test]
fn the_same_seed_gives_the_same_samples() {
    let model = PlayerModel {
        tendency_ms: 10.0,
        sigma_ms: 7.0,
        miss: 0.2,
        double: 0.2,
        seed: 42,
    };
    let go = |model: &PlayerModel| {
        let mut r = FakeRig::new(config(FS, FS, 40.0, 1.0e-3));
        let a = anchor(&r);
        let truth = r.schedule_player_with(
            a,
            &hits(10, HitKind::Strum),
            model,
            &PlayerOptions {
                muted: 0.3,
                ..PlayerOptions::default()
            },
        );
        let (_, tap) = run(&mut r, 7.0);
        (truth, tap)
    };
    let (t1, s1) = go(&model);
    let (t2, s2) = go(&model);
    assert_eq!(t1, t2);
    assert!(s1.len() > 300_000);
    assert!(s1 == s2, "same seed, different samples");
    let other = PlayerModel { seed: 43, ..model };
    let (t3, s3) = go(&other);
    assert_ne!(t1, t3);
    assert!(s1 != s3);
}

#[test]
fn misses_doubles_mutes_and_the_late_start_are_in_the_truth() {
    let mut r = FakeRig::new(config(FS, FS, 40.0, 0.0));
    let a = anchor(&r);
    let h = hits(30, HitKind::Strum);

    // No-strum case: nothing is played, nothing is queued.
    let none = PlayerModel {
        miss: 1.0,
        ..PlayerModel::default()
    };
    let t = r.schedule_player(a, &h, &none);
    assert_eq!(t.len(), 30);
    assert!(
        t.iter()
            .all(|t| !t.played && t.onset_in.is_none() && t.dt_ms.is_none())
    );
    assert_eq!(r.pending_strums(), 0);

    // A late start: the first 4 hits are not played.
    let t = r.schedule_player_with(
        a,
        &h,
        &PlayerModel::default(),
        &PlayerOptions {
            skip: 4,
            ..PlayerOptions::default()
        },
    );
    assert_eq!(played(&t).len(), 26);
    assert!(t[..4].iter().all(|t| !t.played));
    assert_eq!(r.pending_strums(), 26);

    // Misses and doubles at about the stated rates; extras follow the hits in the list.
    let mut r = FakeRig::new(config(FS, FS, 40.0, 0.0));
    let a = anchor(&r);
    let model = PlayerModel {
        miss: 0.3,
        double: 0.4,
        seed: 9,
        ..PlayerModel::default()
    };
    let t = r.schedule_player(a, &hits(30, HitKind::Strum), &model);
    let (hit, extra): (Vec<_>, Vec<_>) = t.iter().partition(|t| !t.extra);
    assert_eq!(hit.len(), 30);
    let missed = hit.iter().filter(|t| !t.played).count();
    assert!((4..=14).contains(&missed), "missed {missed}");
    assert!(!extra.is_empty());
    for e in &extra {
        assert!(e.id.is_none() && e.played);
        let gap = e.dt_ms.unwrap();
        assert!((40.0..=120.0).contains(&gap), "gap {gap}");
    }
    assert_eq!(r.pending_strums(), played(&t).len());

    // Mutes: Mute hits are chucks, open ones are not, ghosts are never strummed, `muted` mixes.
    let mut r = FakeRig::new(config(FS, FS, 40.0, 0.0));
    let a = anchor(&r);
    let mix: Vec<ChartHit> = (0..9)
        .map(|id| ChartHit {
            id,
            t_s: 0.6 * f64::from(id),
            kind: [HitKind::Strum, HitKind::Mute, HitKind::Ghost][id as usize % 3],
        })
        .collect();
    let t = r.schedule_player(a, &mix, &PlayerModel::default());
    for (i, e) in t.iter().enumerate() {
        assert_eq!(e.muted, i % 3 != 0, "hit {i}");
        assert_eq!(e.played, i % 3 != 2, "hit {i}");
    }
    let t = r.schedule_player_with(
        a,
        &hits(20, HitKind::Strum),
        &PlayerModel::default(),
        &PlayerOptions {
            muted: 1.0,
            ..PlayerOptions::default()
        },
    );
    assert!(t.iter().all(|t| t.muted));
}

#[test]
fn feed_wav_delivers_the_file_bit_for_bit() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rig-feed-wav");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("feed.wav");
    // 1.3 s that has no zero and does not repeat: a ramp of odd steps with a sine on top.
    let n = (1.3 * f64::from(FS)) as usize;
    let wav: Vec<f32> = (0..n)
        .map(|i| {
            let x = (i as f32 * 0.037).sin() * 0.4 + ((i * 7919) % 1009) as f32 / 4096.0 + 0.001;
            x.clamp(-0.99, 0.99)
        })
        .collect();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: FS,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for v in &wav {
        w.write_sample(*v).unwrap();
    }
    w.finalize().unwrap();

    let mut r = FakeRig::new(config(FS, FS, 35.0, 0.0));
    r.feed_wav(&path).unwrap();
    let (_, tap) = run(&mut r, 2.0);
    // Where the file starts in the tap: the first sample of the file is not 0.
    let at = tap
        .iter()
        .position(|&v| v == wav[0])
        .expect("the first sample shows up");
    assert!(at + wav.len() <= tap.len());
    assert!(
        tap[at..at + wav.len()] == wav[..],
        "the tap differs from the file"
    );
    // And the silence after it is silence.
    assert!(tap[at + wav.len()..].iter().all(|&v| v == 0.0));

    // A file at another rate is refused.
    let bad = dir.join("rate.wav");
    let spec = hound::WavSpec {
        sample_rate: 44_100,
        ..spec
    };
    let mut w = hound::WavWriter::create(&bad, spec).unwrap();
    w.write_sample(0.0f32).unwrap();
    w.finalize().unwrap();
    assert_eq!(
        r.feed_wav(&bad).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[test]
fn the_switch_for_real_devices_reads_the_environment() {
    // The harness may set the variable; the answer must agree with it.
    assert_eq!(
        real_devices_allowed(),
        std::env::var_os("GUITARNESS_FORBID_DEVICES").is_none()
    );
}

#[test]
fn a_chart_gives_its_hits_and_the_player_follows_it() {
    use guitarness_core::chart::{Chart, Dir, Event, EventKind, Style};
    let mut chart = Chart::new("rig", 120.0);
    let tpq = chart.tpq;
    let strum = |tick, kind| Event {
        tick,
        dur: tpq,
        kind,
        chord: Some("E".into()),
    };
    let down = EventKind::Strum {
        dir: Dir::Down,
        span: None,
        style: Style::Full,
        roll_ms: None,
    };
    chart.tiers[0].events = vec![
        strum(0, down),
        strum(tpq, EventKind::Mute { dir: Dir::Down }),
        strum(2 * tpq, EventKind::Ghost { dir: Dir::Up }),
        strum(3 * tpq, EventKind::Rest),
        strum(4 * tpq, down),
    ];
    let h = guitarness_session::rig::HitSource::hits(&chart);
    assert_eq!(h.len(), 4);
    assert_eq!(
        h.iter().map(|h| (h.id, h.kind)).collect::<Vec<_>>(),
        [
            (0, HitKind::Strum),
            (1, HitKind::Mute),
            (2, HitKind::Ghost),
            (4, HitKind::Strum)
        ]
    );
    assert!((h[3].t_s - 2.0).abs() < 1e-9, "{}", h[3].t_s);

    let mut r = FakeRig::new(config(FS, FS, 40.0, 0.0));
    let a = anchor(&r);
    let truth = r.schedule_player(a, &chart, &PlayerModel::default());
    assert_eq!(truth.iter().filter(|t| t.played).count(), 3);
    let want = a + (2.0 * f64::from(FS)) as u64;
    assert_eq!(truth[3].out_frame, want);
}
